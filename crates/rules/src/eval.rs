use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::types::{
    CanonicalValue, CheckedExpr, CheckedType, CompiledDecisionRow, CompiledDecisionTable,
    CompiledDecisionTestCase, DecisionOutputField, DefinitionRevision, EvaluatedRuleValue,
    RuleArtifact, access_result_type, opaque_json_within_bounds,
};
use crate::{
    BinaryOp, CanonicalRoot, CompiledRule, DecisionConditionTrace, DecisionRejection,
    DecisionResult, DecisionTableDefinition, DecisionTrace, Expr, ExprKind, ExpressionContext,
    Function, HitPolicy, Literal, MAGIC, PROFILE_VERSION, RuleCatalog, RuleDefinition, RuleError,
    RuleType, UnaryOp, parse_expression,
};

pub type RuleBindings = BTreeMap<String, Value>;

/// Type-check a closed catalog at deploy time. Evaluation only receives this
/// compiled representation and explicitly supplied JSON bindings.
pub fn compile_catalog(
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
) -> Result<RuleCatalog, RuleError> {
    compile_catalog_with_declared_types(&BTreeMap::new(), rules, tables)
}

/// Compile a closed rules catalog with the finite named declarations resolved
/// by the deploy-time metadata adapter. This keeps `donat-rules` independent
/// from the metadata crate while making enum symbols nominal at every source
/// site.
pub fn compile_catalog_with_declared_types(
    declared_types: &BTreeMap<String, RuleType>,
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
) -> Result<RuleCatalog, RuleError> {
    compile_catalog_internal(declared_types, None, rules, tables, None, None)
}

/// Compile a catalog with the source-site locations supplied by the metadata
/// adapter. Every rule body and decision-row condition must have one context.
/// The legacy entry points remain context-free for crate consumers that do not
/// originate from metadata.
pub fn compile_catalog_with_declared_types_and_contexts(
    declared_types: &BTreeMap<String, RuleType>,
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
    rule_contexts: &[ExpressionContext],
    decision_condition_contexts: &[Vec<BTreeMap<String, ExpressionContext>>],
) -> Result<RuleCatalog, RuleError> {
    let declaration_order = declared_types.keys().cloned().collect::<Vec<_>>();
    compile_catalog_with_declared_types_and_contexts_and_declaration_order(
        declared_types,
        &declaration_order,
        rules,
        tables,
        rule_contexts,
        decision_condition_contexts,
    )
}

/// Compile a catalog while retaining the metadata declaration list order in
/// canonical decision records. Map-only callers use deterministic UTF-8 key
/// order through [`compile_catalog_with_declared_types_and_contexts`].
pub fn compile_catalog_with_declared_types_and_contexts_and_declaration_order(
    declared_types: &BTreeMap<String, RuleType>,
    declaration_order: &[String],
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
    rule_contexts: &[ExpressionContext],
    decision_condition_contexts: &[Vec<BTreeMap<String, ExpressionContext>>],
) -> Result<RuleCatalog, RuleError> {
    if rule_contexts.len() != rules.len() || decision_condition_contexts.len() != tables.len() {
        return Err(RuleError::InternalInvariant {
            rule: "catalog".to_owned(),
        });
    }
    compile_catalog_internal(
        declared_types,
        Some(declaration_order),
        rules,
        tables,
        Some(rule_contexts),
        Some(decision_condition_contexts),
    )
}

fn compile_catalog_internal(
    declared_types: &BTreeMap<String, RuleType>,
    declaration_order: Option<&[String]>,
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
    rule_contexts: Option<&[ExpressionContext]>,
    decision_condition_contexts: Option<&[Vec<BTreeMap<String, ExpressionContext>>]>,
) -> Result<RuleCatalog, RuleError> {
    let mut catalog = RuleCatalog::default();
    let declared_type_declarations = canonical_declarations(declared_types, declaration_order)?;

    for (rule_index, definition) in rules.iter().enumerate() {
        if catalog.rules.contains_key(&definition.name)
            || catalog.decision_tables.contains_key(&definition.name)
        {
            return Err(RuleError::DuplicateName {
                kind: "rule",
                name: definition.name.clone(),
            });
        }
        let checked = if let Some(contexts) = rule_contexts {
            let context = contexts
                .get(rule_index)
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: definition.name.clone(),
                })?;
            compile_expression_in_context_with_declared_types(
                context,
                &definition.expression,
                &definition.bindings,
                declared_types,
            )?
        } else {
            compile_expression(
                &definition.name,
                &definition.expression,
                &definition.bindings,
                declared_types,
            )?
        };
        if !is_assignable(&checked.type_, &definition.result) {
            let error = RuleError::InvalidRuleResult {
                rule: definition.name.clone(),
                expected: definition.result.display_name(),
                actual: checked_type_name(&checked.type_),
            };
            return Err(
                match rule_contexts.and_then(|contexts| contexts.get(rule_index)) {
                    Some(context) => error.with_diagnostic(context, checked.expression.span),
                    None => error,
                },
            );
        }
        let mut compiled = CompiledRule {
            name: definition.name.clone(),
            bindings: definition.bindings.clone(),
            result: definition.result.clone(),
            artifact: RuleArtifact {
                profile_version: PROFILE_VERSION,
                original_source: definition.expression.clone(),
                canonical_ast_sha256: String::new(),
                source_sha256: sha256_hex(definition.expression.as_bytes()),
            },
            expression: checked,
            declared_types: declared_type_declarations.clone(),
        };
        let bytes = canonical_bytes(
            CanonicalRoot::TypedRuleAst,
            &CanonicalValue::TypedRule(compiled.clone()),
        );
        compiled.artifact.canonical_ast_sha256 = sha256_hex(&bytes);
        catalog.rules.insert(definition.name.clone(), compiled);
    }

    for (table_index, definition) in tables.iter().enumerate() {
        if catalog.rules.contains_key(&definition.name)
            || catalog.decision_tables.contains_key(&definition.name)
        {
            return Err(RuleError::DuplicateName {
                kind: "decision table",
                name: definition.name.clone(),
            });
        }
        let compiled = compile_decision_table(
            definition,
            declared_types,
            &declared_type_declarations,
            decision_condition_contexts.and_then(|contexts| contexts.get(table_index)),
        )?;
        catalog
            .decision_tables
            .insert(definition.name.clone(), compiled);
    }

    validate_decision_test_cases(&catalog, tables)?;
    derive_decision_revisions(&mut catalog);
    Ok(catalog)
}

fn compile_expression(
    rule_name: &str,
    source: &str,
    bindings: &BTreeMap<String, RuleType>,
    declared_types: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError> {
    let expression = parse_expression(rule_name, source)?;
    check_expression(None, rule_name, &expression, bindings, declared_types)
}

fn compile_expression_in_context(
    context: &ExpressionContext,
    source: &str,
    bindings: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError> {
    let expression = parse_expression(context.parser_name(), source)?;
    check_expression(
        Some(context),
        context.parser_name(),
        &expression,
        bindings,
        &BTreeMap::new(),
    )
}

fn compile_expression_in_context_with_declared_types(
    context: &ExpressionContext,
    source: &str,
    bindings: &BTreeMap<String, RuleType>,
    declared_types: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError> {
    if declared_types.is_empty() {
        return compile_expression_in_context(context, source, bindings);
    }
    let expression = parse_expression(context.parser_name(), source)?;
    check_expression(
        Some(context),
        context.parser_name(),
        &expression,
        bindings,
        declared_types,
    )
}

impl RuleCatalog {
    pub fn rule(&self, name: &str) -> Option<&CompiledRule> {
        self.rules.get(name)
    }

    /// Return the compiled, audit-safe decision definition by its stable name.
    /// Row IDs and descriptions are retained for operator diagnostics; typed
    /// source conditions and outputs remain internal to the evaluator.
    pub fn decision_table(&self, name: &str) -> Option<&crate::CompiledDecisionTable> {
        self.decision_tables.get(name)
    }

    pub fn evaluate_bool(
        &self,
        rule: &CompiledRule,
        bindings: &RuleBindings,
    ) -> Result<bool, RuleError> {
        evaluate_bool(rule, bindings)
    }

    pub fn evaluate_decision(
        &self,
        table: &str,
        bindings: &RuleBindings,
    ) -> Result<DecisionResult, RuleError> {
        let compiled =
            self.decision_tables
                .get(table)
                .ok_or_else(|| RuleError::UnknownDecisionTable {
                    table: table.to_owned(),
                })?;
        evaluate_decision_table(compiled, bindings)
    }
}

/// Evaluate a compiled rule over its complete, closed bindings.
pub fn evaluate_value(
    rule: &CompiledRule,
    bindings: &RuleBindings,
) -> Result<EvaluatedRuleValue, RuleError> {
    let value = evaluate_runtime_value(rule, bindings)?;

    Ok(EvaluatedRuleValue {
        type_: value.type_.clone(),
        value: value.value.canonical_json(&rule.name, &value.type_)?,
    })
}

/// Evaluate a compiled boolean rule over its complete, closed bindings.
pub fn evaluate_bool(rule: &CompiledRule, bindings: &RuleBindings) -> Result<bool, RuleError> {
    let value = evaluate_runtime_value(rule, bindings)?;
    if value.type_ != RuleType::Bool {
        return Err(RuleError::InvalidRuleResult {
            rule: rule.name.clone(),
            expected: "bool".to_owned(),
            actual: value.value.type_name(),
        });
    }
    let RuntimeValue::Bool(value) = value.value else {
        return Err(RuleError::InternalInvariant {
            rule: rule.name.clone(),
        });
    };
    Ok(value)
}

struct EvaluatedRuntimeValue {
    type_: RuleType,
    value: RuntimeValue,
}

fn evaluate_runtime_value(
    rule: &CompiledRule,
    bindings: &RuleBindings,
) -> Result<EvaluatedRuntimeValue, RuleError> {
    let context = decode_bindings(&rule.bindings, bindings)?;
    let value = evaluate_expression(&rule.name, &rule.expression.expression, &context)?;

    Ok(EvaluatedRuntimeValue {
        type_: rule.result.clone(),
        value,
    })
}

fn compile_decision_table(
    definition: &DecisionTableDefinition,
    declared_types: &BTreeMap<String, RuleType>,
    declared_type_declarations: &[RuleType],
    condition_contexts: Option<&Vec<BTreeMap<String, ExpressionContext>>>,
) -> Result<CompiledDecisionTable, RuleError> {
    let policy = match &definition.hit_policy {
        HitPolicy::First => HitPolicy::First,
        HitPolicy::Unique => HitPolicy::Unique,
        HitPolicy::Unsupported(value) => {
            return Err(RuleError::UnsupportedHitPolicy {
                table: definition.name.clone(),
                policy: value.clone(),
            });
        }
    };
    if definition.rows.is_empty() {
        return Err(RuleError::EmptyDecisionTable {
            table: definition.name.clone(),
        });
    }
    if definition.output.is_empty() {
        return Err(RuleError::InvalidDecisionColumn {
            table: definition.name.clone(),
            column: "output".to_owned(),
        });
    }

    let mut seen_rows = BTreeSet::new();
    let mut rows = Vec::with_capacity(definition.rows.len());
    for (row_index, row) in definition.rows.iter().enumerate() {
        if row.id.is_empty() || !seen_rows.insert(row.id.clone()) {
            return Err(RuleError::InvalidDecisionRow {
                table: definition.name.clone(),
                row_id: row.id.clone(),
            });
        }
        validate_exact_columns(&definition.name, row.when.keys(), definition.inputs.keys())?;
        validate_row_output(definition, row)?;

        let mut conditions = BTreeMap::new();
        for (input, source) in &row.when {
            let checked = if let Some(contexts) = condition_contexts {
                let context = contexts
                    .get(row_index)
                    .and_then(|row_contexts| row_contexts.get(input))
                    .ok_or_else(|| RuleError::InternalInvariant {
                        rule: definition.name.clone(),
                    })?;
                compile_expression_in_context_with_declared_types(
                    context,
                    source,
                    &definition.inputs,
                    declared_types,
                )?
            } else {
                compile_expression(&definition.name, source, &definition.inputs, declared_types)?
            };
            if !matches!(checked.type_, CheckedType::Concrete(RuleType::Bool)) {
                let error = RuleError::InvalidDecisionRow {
                    table: definition.name.clone(),
                    row_id: row.id.clone(),
                };
                let context = condition_contexts
                    .and_then(|contexts| contexts.get(row_index))
                    .and_then(|row_contexts| row_contexts.get(input));
                return Err(match context {
                    Some(context) => error.with_diagnostic(context, checked.expression.span),
                    None => error,
                });
            }
            conditions.insert(input.clone(), checked);
        }
        rows.push(CompiledDecisionRow {
            id: row.id.clone(),
            description: row.description.clone(),
            conditions,
            output: row.output.clone(),
        });
    }

    if matches!(policy, HitPolicy::First)
        && !rows.last().is_some_and(|row| {
            definition.inputs.keys().all(|input| {
                row.conditions.get(input).is_some_and(|condition| {
                    matches!(
                        condition.expression.kind,
                        ExprKind::Literal(Literal::Bool(true))
                    )
                })
            })
        })
    {
        return Err(RuleError::MissingDefaultRow {
            table: definition.name.clone(),
        });
    }

    let test_cases = definition
        .test_cases
        .iter()
        .map(|case| {
            let Value::Object(input) = &case.input else {
                return Err(RuleError::DecisionTestCaseMismatch {
                    table: definition.name.clone(),
                    case_name: case.name.clone(),
                });
            };
            let Value::Object(output) = &case.expect.output else {
                return Err(RuleError::DecisionTestCaseMismatch {
                    table: definition.name.clone(),
                    case_name: case.name.clone(),
                });
            };
            Ok(CompiledDecisionTestCase {
                name: case.name.clone(),
                input: input.clone().into_iter().collect(),
                output: output.clone().into_iter().collect(),
                matched_row_id: case.expect.matched_row_id.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CompiledDecisionTable {
        name: definition.name.clone(),
        revision: DefinitionRevision(String::new()),
        inputs: definition.inputs.clone(),
        output: definition.output.clone(),
        output_fields: definition
            .output
            .iter()
            .map(|(name, type_)| {
                (
                    name.clone(),
                    DecisionOutputField {
                        name: name.clone(),
                        type_: type_.clone(),
                    },
                )
            })
            .collect(),
        hit_policy: policy,
        rows,
        test_cases,
        declared_types: declared_type_declarations.to_vec(),
    })
}

fn validate_exact_columns<'a>(
    table: &str,
    actual: impl Iterator<Item = &'a String>,
    expected: impl Iterator<Item = &'a String>,
) -> Result<(), RuleError> {
    let actual = actual.cloned().collect::<BTreeSet<_>>();
    let expected = expected.cloned().collect::<BTreeSet<_>>();
    if actual == expected {
        return Ok(());
    }
    let column = actual
        .symmetric_difference(&expected)
        .next()
        .cloned()
        .unwrap_or_else(|| "unnamed".to_owned());
    Err(RuleError::InvalidDecisionColumn {
        table: table.to_owned(),
        column,
    })
}

fn validate_row_output(
    definition: &DecisionTableDefinition,
    row: &crate::DecisionRow,
) -> Result<(), RuleError> {
    let Value::Object(output) = &row.output else {
        return Err(RuleError::InvalidDecisionRow {
            table: definition.name.clone(),
            row_id: row.id.clone(),
        });
    };
    validate_exact_columns(&definition.name, output.keys(), definition.output.keys())?;
    for (field, type_) in &definition.output {
        let Some(value) = output.get(field) else {
            return Err(RuleError::InvalidDecisionColumn {
                table: definition.name.clone(),
                column: field.clone(),
            });
        };
        if decode_value(value, type_).is_none() {
            return Err(RuleError::InvalidDecisionRow {
                table: definition.name.clone(),
                row_id: row.id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_decision_test_cases(
    catalog: &RuleCatalog,
    definitions: &[DecisionTableDefinition],
) -> Result<(), RuleError> {
    for definition in definitions {
        for case in &definition.test_cases {
            let Value::Object(input) = &case.input else {
                return Err(RuleError::DecisionTestCaseMismatch {
                    table: definition.name.clone(),
                    case_name: case.name.clone(),
                });
            };
            let bindings = input
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            let result = catalog
                .evaluate_decision(&definition.name, &bindings)
                .map_err(|_| RuleError::DecisionTestCaseMismatch {
                    table: definition.name.clone(),
                    case_name: case.name.clone(),
                })?;
            if result.output != case.expect.output
                || result.matched_row_id != case.expect.matched_row_id
            {
                return Err(RuleError::DecisionTestCaseMismatch {
                    table: definition.name.clone(),
                    case_name: case.name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn check_expression(
    context: Option<&ExpressionContext>,
    rule_name: &str,
    expression: &Expr,
    bindings: &BTreeMap<String, RuleType>,
    declared_types: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError> {
    (|| {
        let type_ = match &expression.kind {
            ExprKind::Literal(literal) => check_literal(rule_name, literal)?,
            ExprKind::Name(name) => {
                CheckedType::Concrete(bindings.get(name).cloned().ok_or_else(|| {
                    RuleError::UndeclaredName {
                        rule: rule_name.to_owned(),
                        name: name.clone(),
                    }
                })?)
            }
            ExprKind::EnumSymbol { enum_name, symbol } => {
                let Some(type_) = declared_types.get(enum_name) else {
                    return Err(RuleError::UnknownEnumType {
                        rule: rule_name.to_owned(),
                        enum_name: enum_name.clone(),
                    });
                };
                let RuleType::Enum { name, symbols } = type_ else {
                    return Err(RuleError::UnknownEnumType {
                        rule: rule_name.to_owned(),
                        enum_name: enum_name.clone(),
                    });
                };
                if name != enum_name || !symbols.iter().any(|candidate| candidate == symbol) {
                    return Err(RuleError::UnknownEnumSymbol {
                        rule: rule_name.to_owned(),
                        enum_name: enum_name.clone(),
                        symbol: symbol.clone(),
                    });
                }
                CheckedType::Concrete(type_.clone())
            }
            ExprKind::List(items) => {
                check_list(context, rule_name, items, bindings, declared_types)?
            }
            ExprKind::Member { target, field } => {
                let target =
                    check_expression(context, rule_name, target, bindings, declared_types)?;
                let object = required_concrete(rule_name, &target.type_)?;
                match object {
                    RuleType::Object { fields, .. } => CheckedType::Concrete(access_result_type(
                        fields.get(field).ok_or_else(|| RuleError::UnknownField {
                            rule: rule_name.to_owned(),
                            field: field.clone(),
                        })?,
                    )),
                    other => return Err(type_mismatch(rule_name, "object", other.display_name())),
                }
            }
            ExprKind::Index { target, index: _ } => {
                let target =
                    check_expression(context, rule_name, target, bindings, declared_types)?;
                let list = required_concrete(rule_name, &target.type_)?;
                match list {
                    RuleType::List(item) => CheckedType::Concrete(access_result_type(item)),
                    other => return Err(type_mismatch(rule_name, "list", other.display_name())),
                }
            }
            ExprKind::Call {
                function,
                arguments,
            } => check_call(
                context,
                rule_name,
                *function,
                arguments,
                bindings,
                declared_types,
            )?,
            ExprKind::Unary { op, operand } => {
                let operand =
                    check_expression(context, rule_name, operand, bindings, declared_types)?;
                let operand = required_concrete(rule_name, &operand.type_)?;
                match op {
                    UnaryOp::Not if matches!(operand, RuleType::Bool) => {
                        CheckedType::Concrete(RuleType::Bool)
                    }
                    UnaryOp::Negate if is_numeric(operand) => {
                        CheckedType::Concrete(operand.clone())
                    }
                    UnaryOp::Not => {
                        return Err(type_mismatch(rule_name, "bool", operand.display_name()));
                    }
                    UnaryOp::Negate => {
                        return Err(type_mismatch(
                            rule_name,
                            "int or decimal",
                            operand.display_name(),
                        ));
                    }
                }
            }
            ExprKind::Binary { op, left, right } => {
                let left = check_expression(context, rule_name, left, bindings, declared_types)?;
                let right = check_expression(context, rule_name, right, bindings, declared_types)?;
                if matches!(op, BinaryOp::Divide) && literal_normalizes_to_zero(&right.expression) {
                    return Err(RuleError::DivisionByZero {
                        rule: rule_name.to_owned(),
                    });
                }
                check_binary(rule_name, *op, &left.type_, &right.type_)?
            }
            ExprKind::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                let condition =
                    check_expression(context, rule_name, condition, bindings, declared_types)?;
                expect_bool(rule_name, &condition.type_)?;
                let when_true =
                    check_expression(context, rule_name, when_true, bindings, declared_types)?;
                let when_false =
                    check_expression(context, rule_name, when_false, bindings, declared_types)?;
                merge_branch_types(rule_name, &when_true.type_, &when_false.type_)?
            }
        };
        Ok(CheckedExpr {
            expression: expression.clone(),
            type_,
        })
    })()
    .map_err(|error| match context {
        Some(context) => error.with_diagnostic(context, expression.span),
        None => error,
    })
}

fn literal_normalizes_to_zero(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Literal(Literal::Int(value)) => {
            value.parse::<i128>().is_ok_and(|value| value == 0)
        }
        ExprKind::Literal(Literal::Decimal(value)) => {
            Decimal::parse(value).is_some_and(|value| value.is_zero())
        }
        ExprKind::Unary {
            op: UnaryOp::Negate,
            operand,
        } => literal_normalizes_to_zero(operand),
        _ => false,
    }
}

fn check_literal(rule_name: &str, literal: &Literal) -> Result<CheckedType, RuleError> {
    match literal {
        Literal::Null => Ok(CheckedType::Null),
        Literal::Bool(_) => Ok(CheckedType::Concrete(RuleType::Bool)),
        Literal::String(_) => Ok(CheckedType::Concrete(RuleType::String)),
        Literal::Int(value) => {
            let type_ = if value.parse::<i32>().is_ok() {
                RuleType::Int
            } else if value.parse::<i64>().is_ok() {
                RuleType::Int64
            } else {
                return Err(RuleError::InvalidLiteral {
                    rule: rule_name.to_owned(),
                    expected: "int or bigint".to_owned(),
                });
            };
            Ok(CheckedType::Concrete(type_))
        }
        Literal::Decimal(value) => Decimal::parse(value)
            .map(|_| CheckedType::Concrete(RuleType::Decimal))
            .ok_or_else(|| RuleError::InvalidLiteral {
                rule: rule_name.to_owned(),
                expected: "decimal".to_owned(),
            }),
    }
}

fn check_list(
    context: Option<&ExpressionContext>,
    rule_name: &str,
    items: &[Expr],
    bindings: &BTreeMap<String, RuleType>,
    declared_types: &BTreeMap<String, RuleType>,
) -> Result<CheckedType, RuleError> {
    let Some((first, rest)) = items.split_first() else {
        return Err(RuleError::InvalidLiteral {
            rule: rule_name.to_owned(),
            expected: "a non-empty typed list".to_owned(),
        });
    };
    let first = check_expression(context, rule_name, first, bindings, declared_types)?;
    let CheckedType::Concrete(item_type) = first.type_ else {
        return Err(RuleError::InvalidLiteral {
            rule: rule_name.to_owned(),
            expected: "a non-null typed list item".to_owned(),
        });
    };
    for item in rest {
        let item = check_expression(context, rule_name, item, bindings, declared_types)?;
        if item.type_ != CheckedType::Concrete(item_type.clone()) {
            return Err(type_mismatch(
                rule_name,
                item_type.display_name(),
                checked_type_name(&item.type_),
            ));
        }
    }
    Ok(CheckedType::Concrete(RuleType::List(Box::new(item_type))))
}

fn check_call(
    context: Option<&ExpressionContext>,
    rule_name: &str,
    function: Function,
    arguments: &[Expr],
    bindings: &BTreeMap<String, RuleType>,
    declared_types: &BTreeMap<String, RuleType>,
) -> Result<CheckedType, RuleError> {
    let checked = arguments
        .iter()
        .map(|argument| check_expression(context, rule_name, argument, bindings, declared_types))
        .collect::<Result<Vec<_>, _>>()?;
    match function {
        Function::Size if checked.len() == 1 => {
            let type_ = required_concrete(rule_name, &checked[0].type_)?;
            match type_ {
                RuleType::String | RuleType::List(_) => Ok(CheckedType::Concrete(RuleType::Int)),
                other => Err(type_mismatch(
                    rule_name,
                    "string or list",
                    other.display_name(),
                )),
            }
        }
        Function::IsNull if checked.len() == 1 => match &checked[0].type_ {
            CheckedType::Concrete(type_) if type_.accepts_null() => {
                Ok(CheckedType::Concrete(RuleType::Bool))
            }
            other => Err(type_mismatch(
                rule_name,
                "nullable value",
                checked_type_name(other),
            )),
        },
        Function::StartsWith | Function::EndsWith if checked.len() == 2 => {
            for argument in &checked {
                let type_ = required_concrete(rule_name, &argument.type_)?;
                if !matches!(type_, RuleType::String) {
                    return Err(type_mismatch(rule_name, "string", type_.display_name()));
                }
            }
            Ok(CheckedType::Concrete(RuleType::Bool))
        }
        Function::Size => Err(type_mismatch(rule_name, "one function argument", "other")),
        Function::IsNull => Err(type_mismatch(rule_name, "one function argument", "other")),
        Function::StartsWith | Function::EndsWith => {
            Err(type_mismatch(rule_name, "two function arguments", "other"))
        }
    }
}

fn check_binary(
    rule_name: &str,
    operation: BinaryOp,
    left: &CheckedType,
    right: &CheckedType,
) -> Result<CheckedType, RuleError> {
    match operation {
        BinaryOp::And | BinaryOp::Or => {
            expect_bool(rule_name, left)?;
            expect_bool(rule_name, right)?;
            Ok(CheckedType::Concrete(RuleType::Bool))
        }
        BinaryOp::Equal | BinaryOp::NotEqual => {
            check_equality(rule_name, left, right)?;
            Ok(CheckedType::Concrete(RuleType::Bool))
        }
        BinaryOp::LessThan
        | BinaryOp::LessThanOrEqual
        | BinaryOp::GreaterThan
        | BinaryOp::GreaterThanOrEqual => {
            let left = required_concrete(rule_name, left)?;
            let right = required_concrete(rule_name, right)?;
            if (left == right && supports_ordering(left)) || is_mixed_integer_pair(left, right) {
                Ok(CheckedType::Concrete(RuleType::Bool))
            } else {
                Err(type_mismatch(
                    rule_name,
                    "matching int, decimal, or timestamp operands",
                    format!("{} and {}", left.display_name(), right.display_name()),
                ))
            }
        }
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
            let left = required_concrete(rule_name, left)?;
            let right = required_concrete(rule_name, right)?;
            if left == right && is_numeric(left) {
                Ok(CheckedType::Concrete(left.clone()))
            } else if is_mixed_integer_pair(left, right) {
                Ok(CheckedType::Concrete(RuleType::Int64))
            } else {
                Err(type_mismatch(
                    rule_name,
                    "matching int or decimal operands",
                    format!("{} and {}", left.display_name(), right.display_name()),
                ))
            }
        }
    }
}

fn check_equality(
    rule_name: &str,
    left: &CheckedType,
    right: &CheckedType,
) -> Result<(), RuleError> {
    match (left, right) {
        (CheckedType::Null, CheckedType::Concrete(other))
        | (CheckedType::Concrete(other), CheckedType::Null)
            if other.accepts_null() =>
        {
            Ok(())
        }
        (CheckedType::Null, _) | (_, CheckedType::Null) => Err(type_mismatch(
            rule_name,
            "a nullable value",
            "non-nullable value",
        )),
        (CheckedType::Concrete(left), CheckedType::Concrete(right))
            if left.accepts_null() || right.accepts_null() =>
        {
            Err(RuleError::NullableOperation {
                rule: rule_name.to_owned(),
            })
        }
        (CheckedType::Concrete(left), CheckedType::Concrete(right))
            if left == right && supports_whole_value_equality(left) =>
        {
            Ok(())
        }
        (CheckedType::Concrete(left), CheckedType::Concrete(right))
            if is_mixed_integer_pair(left, right) =>
        {
            Ok(())
        }
        _ => Err(type_mismatch(
            rule_name,
            checked_type_name(left),
            checked_type_name(right),
        )),
    }
}

fn supports_whole_value_equality(type_: &RuleType) -> bool {
    !matches!(
        type_,
        RuleType::List(_) | RuleType::Object { .. } | RuleType::OpaqueJson { .. }
    )
}

fn supports_ordering(type_: &RuleType) -> bool {
    is_numeric(type_) || matches!(type_, RuleType::Timestamp)
}

fn merge_branch_types(
    rule_name: &str,
    when_true: &CheckedType,
    when_false: &CheckedType,
) -> Result<CheckedType, RuleError> {
    match (when_true, when_false) {
        (CheckedType::Concrete(left), CheckedType::Concrete(right)) if left == right => {
            Ok(CheckedType::Concrete(left.clone()))
        }
        (CheckedType::Concrete(left), CheckedType::Concrete(right))
            if is_mixed_integer_pair(left, right) =>
        {
            Ok(CheckedType::Concrete(RuleType::Int64))
        }
        (CheckedType::Null, CheckedType::Concrete(other))
        | (CheckedType::Concrete(other), CheckedType::Null)
            if other.accepts_null() =>
        {
            Ok(CheckedType::Concrete(other.clone()))
        }
        _ => Err(RuleError::IncompatibleBranches {
            rule: rule_name.to_owned(),
        }),
    }
}

fn expect_bool(rule_name: &str, type_: &CheckedType) -> Result<(), RuleError> {
    let type_ = required_concrete(rule_name, type_)?;
    if matches!(type_, RuleType::Bool) {
        Ok(())
    } else {
        Err(type_mismatch(rule_name, "bool", type_.display_name()))
    }
}

fn required_concrete<'a>(
    rule_name: &str,
    type_: &'a CheckedType,
) -> Result<&'a RuleType, RuleError> {
    match type_ {
        CheckedType::Concrete(type_) if type_.accepts_null() => Err(RuleError::NullableOperation {
            rule: rule_name.to_owned(),
        }),
        CheckedType::Concrete(type_) => Ok(type_),
        CheckedType::Null => Err(type_mismatch(rule_name, "a non-null value", "null")),
    }
}

fn is_assignable(actual: &CheckedType, expected: &RuleType) -> bool {
    match actual {
        CheckedType::Null => expected.accepts_null(),
        CheckedType::Concrete(actual) => {
            actual == expected || matches!((actual, expected), (RuleType::Int, RuleType::Int64))
        }
    }
}

fn checked_type_name(type_: &CheckedType) -> String {
    match type_ {
        CheckedType::Concrete(type_) => type_.display_name(),
        CheckedType::Null => "null".to_owned(),
    }
}

fn type_mismatch(
    rule_name: &str,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> RuleError {
    RuleError::TypeMismatch {
        rule: rule_name.to_owned(),
        expected: expected.into(),
        actual: actual.into(),
    }
}

fn is_numeric(type_: &RuleType) -> bool {
    matches!(type_, RuleType::Int | RuleType::Int64 | RuleType::Decimal)
}

fn is_mixed_integer_pair(left: &RuleType, right: &RuleType) -> bool {
    matches!(
        (left, right),
        (RuleType::Int, RuleType::Int64) | (RuleType::Int64, RuleType::Int)
    )
}

fn evaluate_decision_table(
    table: &CompiledDecisionTable,
    bindings: &RuleBindings,
) -> Result<DecisionResult, RuleError> {
    let context = decode_bindings(&table.inputs, bindings)?;
    let mut matched = Vec::new();
    let mut condition_results = Vec::with_capacity(table.rows.len());

    for row in &table.rows {
        let mut conditions = BTreeMap::new();
        let mut is_match = true;
        for (input, expression) in &row.conditions {
            let value = evaluate_expression(&table.name, &expression.expression, &context)?;
            let RuntimeValue::Bool(value) = value else {
                return Err(RuleError::InvalidDecisionRow {
                    table: table.name.clone(),
                    row_id: row.id.clone(),
                });
            };
            conditions.insert(input.clone(), value);
            is_match &= value;
        }
        if is_match {
            matched.push(row);
        }
        condition_results.push(DecisionConditionTrace {
            row_id: row.id.clone(),
            conditions,
        });
    }

    let selected = match &table.hit_policy {
        HitPolicy::First => matched
            .first()
            .copied()
            .ok_or_else(|| RuleError::DecisionNoMatch {
                table: table.name.clone(),
                trace: Box::new(decision_trace(
                    table,
                    None,
                    Some(DecisionRejection::NoMatch),
                    condition_results.clone(),
                    &context,
                )),
            })?,
        HitPolicy::Unique if matched.len() == 1 => {
            matched
                .first()
                .copied()
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: table.name.clone(),
                })?
        }
        HitPolicy::Unique if matched.is_empty() => {
            return Err(RuleError::DecisionNoMatch {
                table: table.name.clone(),
                trace: Box::new(decision_trace(
                    table,
                    None,
                    Some(DecisionRejection::NoMatch),
                    condition_results,
                    &context,
                )),
            });
        }
        HitPolicy::Unique => {
            return Err(RuleError::DecisionMultipleMatches {
                table: table.name.clone(),
                row_ids: matched.iter().map(|row| row.id.clone()).collect(),
                trace: Box::new(decision_trace(
                    table,
                    None,
                    Some(DecisionRejection::MultipleMatches),
                    condition_results,
                    &context,
                )),
            });
        }
        HitPolicy::Unsupported(policy) => {
            return Err(RuleError::UnsupportedHitPolicy {
                table: table.name.clone(),
                policy: policy.clone(),
            });
        }
    };
    let matched_row_id = selected.id.clone();
    Ok(DecisionResult {
        output: selected.output.clone(),
        matched_row_id: matched_row_id.clone(),
        trace: decision_trace(
            table,
            Some(matched_row_id),
            None,
            condition_results,
            &context,
        ),
    })
}

fn decision_trace(
    table: &CompiledDecisionTable,
    matched_row_id: Option<String>,
    rejection: Option<DecisionRejection>,
    condition_results: Vec<DecisionConditionTrace>,
    decoded_bindings: &BTreeMap<String, RuntimeValue>,
) -> DecisionTrace {
    DecisionTrace {
        table_name: table.name.clone(),
        table_revision: table.revision.0.clone(),
        matched_row_id,
        rejection,
        condition_results,
        input_digest: sha256_hex(&canonical_decoded_input_bytes(
            &table.inputs,
            decoded_bindings,
        )),
    }
}

fn decode_bindings(
    declared: &BTreeMap<String, RuleType>,
    bindings: &RuleBindings,
) -> Result<BTreeMap<String, RuntimeValue>, RuleError> {
    for name in declared.keys() {
        if !bindings.contains_key(name) {
            return Err(RuleError::MissingBinding { name: name.clone() });
        }
    }
    for name in bindings.keys() {
        if !declared.contains_key(name) {
            return Err(RuleError::UnexpectedBinding { name: name.clone() });
        }
    }
    let mut decoded = BTreeMap::new();
    for (name, type_) in declared {
        let value = bindings
            .get(name)
            .ok_or_else(|| RuleError::MissingBinding { name: name.clone() })?;
        let value = decode_value(value, type_).ok_or_else(|| RuleError::InvalidBinding {
            name: name.clone(),
            expected: type_.display_name(),
        })?;
        decoded.insert(name.clone(), value);
    }
    Ok(decoded)
}

fn decode_value(value: &Value, type_: &RuleType) -> Option<RuntimeValue> {
    match type_ {
        RuleType::Nullable(inner) if value.is_null() => Some(RuntimeValue::Null),
        RuleType::Nullable(inner) => decode_value(value, inner),
        RuleType::Bool => value.as_bool().map(RuntimeValue::Bool),
        RuleType::String => value
            .as_str()
            .map(|value| RuntimeValue::String(value.to_owned())),
        RuleType::Int => value
            .as_number()
            .and_then(|number| number.to_string().parse::<i32>().ok())
            .map(RuntimeValue::Int),
        RuleType::Int64 => value
            .as_number()
            .and_then(|number| number.to_string().parse::<i64>().ok())
            .map(RuntimeValue::Int64),
        RuleType::Decimal => value
            .as_number()
            .and_then(|number| Decimal::parse(&number.to_string()))
            .map(RuntimeValue::Decimal),
        RuleType::Uuid => value
            .as_str()
            .filter(|value| is_uuid(value))
            .map(|value| RuntimeValue::Uuid(value.to_ascii_lowercase())),
        RuleType::Date => value
            .as_str()
            .filter(|value| is_canonical_date(value))
            .map(|value| RuntimeValue::Date(value.to_owned())),
        RuleType::Timestamp => value
            .as_str()
            .filter(|value| is_canonical_timestamp(value))
            .and_then(canonical_timestamp_utc)
            .map(RuntimeValue::Timestamp),
        RuleType::Enum { name, symbols } => value
            .as_str()
            .filter(|value| symbols.iter().any(|symbol| symbol == value))
            .map(|value| RuntimeValue::Enum {
                enum_name: name.clone(),
                symbol: value.to_owned(),
            }),
        RuleType::List(item_type) => value.as_array().and_then(|items| {
            items
                .iter()
                .map(|item| decode_value(item, item_type))
                .collect::<Option<Vec<_>>>()
                .map(RuntimeValue::List)
        }),
        RuleType::Object { fields, .. } => value.as_object().and_then(|object| {
            fields
                .iter()
                .map(|(field, type_)| {
                    let value = match object.get(field) {
                        Some(value) => decode_value(value, type_),
                        None => Some(RuntimeValue::Null),
                    }?;
                    Some((field.clone(), value))
                })
                .collect::<Option<BTreeMap<_, _>>>()
                .map(RuntimeValue::Object)
        }),
        RuleType::OpaqueJson {
            maximum_bytes,
            maximum_depth,
            maximum_nodes,
            ..
        } if opaque_json_within_bounds(value, *maximum_bytes, *maximum_depth, *maximum_nodes) => {
            Some(RuntimeValue::OpaqueJson(value.clone()))
        }
        RuleType::OpaqueJson { .. } => None,
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
        })
}

fn is_canonical_date(value: &str) -> bool {
    if !value.is_ascii() || value.len() != 10 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return false;
    }
    let year = parse_two_or_four_digits(&value[0..4]);
    let month = parse_two_or_four_digits(&value[5..7]);
    let day = parse_two_or_four_digits(&value[8..10]);
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    year > 0 && month > 0 && month <= 12 && day > 0 && day <= days_in_month(year, month)
}

fn is_canonical_timestamp(value: &str) -> bool {
    if !value.is_ascii() || value.len() < 20 || !is_canonical_date(&value[0..10]) {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !bytes[11..13].iter().all(u8::is_ascii_digit)
        || !bytes[14..16].iter().all(u8::is_ascii_digit)
        || !bytes[17..19].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let (Some(hour), Some(minute), Some(second)) = (
        parse_two_or_four_digits(&value[11..13]),
        parse_two_or_four_digits(&value[14..16]),
        parse_two_or_four_digits(&value[17..19]),
    ) else {
        return false;
    };
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }

    let mut zone_start = 19;
    if bytes.get(zone_start) == Some(&b'.') {
        zone_start += 1;
        let fractional_start = zone_start;
        while bytes
            .get(zone_start)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            zone_start += 1;
        }
        if zone_start == fractional_start {
            return false;
        }
    }
    match bytes.get(zone_start) {
        Some(b'Z') => zone_start + 1 == bytes.len(),
        Some(b'+') | Some(b'-') if zone_start + 6 == bytes.len() => {
            bytes[zone_start + 3] == b':'
                && bytes[zone_start + 1..zone_start + 3]
                    .iter()
                    .all(u8::is_ascii_digit)
                && bytes[zone_start + 4..zone_start + 6]
                    .iter()
                    .all(u8::is_ascii_digit)
                && parse_two_or_four_digits(&value[zone_start + 1..zone_start + 3])
                    .is_some_and(|offset_hour| offset_hour <= 23)
                && parse_two_or_four_digits(&value[zone_start + 4..zone_start + 6])
                    .is_some_and(|offset_minute| offset_minute <= 59)
        }
        _ => false,
    }
}

fn canonical_timestamp_utc(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut zone_start = 19;
    if bytes.get(zone_start) == Some(&b'.') {
        zone_start += 1;
        while bytes
            .get(zone_start)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            zone_start += 1;
        }
    }
    let fraction = value[19..zone_start]
        .trim_end_matches('0')
        .trim_end_matches('.');
    if bytes.get(zone_start) == Some(&b'Z') {
        return Some(format!("{}{fraction}Z", &value[..19]));
    }
    let sign = match bytes.get(zone_start) {
        Some(b'+') => 1_i32,
        Some(b'-') => -1_i32,
        _ => return None,
    };
    let offset_hour = parse_two_or_four_digits(&value[zone_start + 1..zone_start + 3])? as i32;
    let offset_minute = parse_two_or_four_digits(&value[zone_start + 4..zone_start + 6])? as i32;
    let mut year = parse_two_or_four_digits(&value[0..4])?;
    let mut month = parse_two_or_four_digits(&value[5..7])?;
    let mut day = parse_two_or_four_digits(&value[8..10])?;
    let local_minutes = parse_two_or_four_digits(&value[11..13])? as i32 * 60
        + parse_two_or_four_digits(&value[14..16])? as i32;
    let mut utc_minutes = local_minutes - sign * (offset_hour * 60 + offset_minute);
    while utc_minutes < 0 {
        utc_minutes += 24 * 60;
        if day > 1 {
            day -= 1;
        } else if month > 1 {
            month -= 1;
            day = days_in_month(year, month);
        } else {
            year = year.checked_sub(1)?;
            if year == 0 {
                return None;
            }
            month = 12;
            day = 31;
        }
    }
    while utc_minutes >= 24 * 60 {
        utc_minutes -= 24 * 60;
        if day < days_in_month(year, month) {
            day += 1;
        } else if month < 12 {
            month += 1;
            day = 1;
        } else {
            year = year.checked_add(1)?;
            month = 1;
            day = 1;
        }
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{}{fraction}Z",
        utc_minutes / 60,
        utc_minutes % 60,
        &value[17..19],
    ))
}

fn parse_two_or_four_digits(value: &str) -> Option<u32> {
    value.parse::<u32>().ok()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn evaluate_expression(
    rule_name: &str,
    expression: &Expr,
    context: &BTreeMap<String, RuntimeValue>,
) -> Result<RuntimeValue, RuleError> {
    match &expression.kind {
        ExprKind::Literal(literal) => evaluate_literal(rule_name, literal),
        ExprKind::Name(name) => context
            .get(name)
            .cloned()
            .ok_or_else(|| RuleError::MissingBinding { name: name.clone() }),
        ExprKind::EnumSymbol { enum_name, symbol } => Ok(RuntimeValue::Enum {
            enum_name: enum_name.clone(),
            symbol: symbol.clone(),
        }),
        ExprKind::List(items) => items
            .iter()
            .map(|item| evaluate_expression(rule_name, item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(RuntimeValue::List),
        ExprKind::Member { target, field } => {
            match evaluate_expression(rule_name, target, context)? {
                RuntimeValue::Object(fields) => {
                    Ok(fields.get(field).cloned().unwrap_or(RuntimeValue::Null))
                }
                other => Err(type_mismatch(rule_name, "object", other.type_name())),
            }
        }
        ExprKind::Index { target, index } => match evaluate_expression(rule_name, target, context)?
        {
            RuntimeValue::List(items) => {
                Ok(items.get(*index).cloned().unwrap_or(RuntimeValue::Null))
            }
            other => Err(type_mismatch(rule_name, "list", other.type_name())),
        },
        ExprKind::Call {
            function,
            arguments,
        } => evaluate_call(rule_name, *function, arguments, context),
        ExprKind::Unary { op, operand } => {
            let operand = evaluate_expression(rule_name, operand, context)?;
            match (op, operand) {
                (UnaryOp::Not, RuntimeValue::Bool(value)) => Ok(RuntimeValue::Bool(!value)),
                (UnaryOp::Negate, RuntimeValue::Int(value)) => value
                    .checked_neg()
                    .map(RuntimeValue::Int)
                    .ok_or_else(|| arithmetic_overflow(rule_name)),
                (UnaryOp::Negate, RuntimeValue::Int64(value)) => value
                    .checked_neg()
                    .map(RuntimeValue::Int64)
                    .ok_or_else(|| arithmetic_overflow(rule_name)),
                (UnaryOp::Negate, RuntimeValue::Decimal(value)) => value
                    .checked_neg()
                    .map(RuntimeValue::Decimal)
                    .ok_or_else(|| arithmetic_overflow(rule_name)),
                (UnaryOp::Not, other) => Err(type_mismatch(rule_name, "bool", other.type_name())),
                (UnaryOp::Negate, other) => Err(type_mismatch(
                    rule_name,
                    "int or decimal",
                    other.type_name(),
                )),
            }
        }
        ExprKind::Binary { op, left, right } => {
            if matches!(op, BinaryOp::And) {
                let RuntimeValue::Bool(left) = evaluate_expression(rule_name, left, context)?
                else {
                    return Err(type_mismatch(rule_name, "bool", "non-bool"));
                };
                if !left {
                    return Ok(RuntimeValue::Bool(false));
                }
                let RuntimeValue::Bool(right) = evaluate_expression(rule_name, right, context)?
                else {
                    return Err(type_mismatch(rule_name, "bool", "non-bool"));
                };
                return Ok(RuntimeValue::Bool(right));
            }
            if matches!(op, BinaryOp::Or) {
                let RuntimeValue::Bool(left) = evaluate_expression(rule_name, left, context)?
                else {
                    return Err(type_mismatch(rule_name, "bool", "non-bool"));
                };
                if left {
                    return Ok(RuntimeValue::Bool(true));
                }
                let RuntimeValue::Bool(right) = evaluate_expression(rule_name, right, context)?
                else {
                    return Err(type_mismatch(rule_name, "bool", "non-bool"));
                };
                return Ok(RuntimeValue::Bool(right));
            }
            let left = evaluate_expression(rule_name, left, context)?;
            let right = evaluate_expression(rule_name, right, context)?;
            evaluate_binary(rule_name, *op, left, right)
        }
        ExprKind::Conditional {
            condition,
            when_true,
            when_false,
        } => match evaluate_expression(rule_name, condition, context)? {
            RuntimeValue::Bool(true) => evaluate_expression(rule_name, when_true, context),
            RuntimeValue::Bool(false) => evaluate_expression(rule_name, when_false, context),
            other => Err(type_mismatch(rule_name, "bool", other.type_name())),
        },
    }
}

fn evaluate_literal(rule_name: &str, literal: &Literal) -> Result<RuntimeValue, RuleError> {
    match literal {
        Literal::Null => Ok(RuntimeValue::Null),
        Literal::Bool(value) => Ok(RuntimeValue::Bool(*value)),
        Literal::String(value) => Ok(RuntimeValue::String(value.clone())),
        Literal::Int(value) => {
            if let Ok(value) = value.parse::<i32>() {
                Ok(RuntimeValue::Int(value))
            } else if let Ok(value) = value.parse::<i64>() {
                Ok(RuntimeValue::Int64(value))
            } else {
                Err(RuleError::InvalidLiteral {
                    rule: rule_name.to_owned(),
                    expected: "int or bigint".to_owned(),
                })
            }
        }
        Literal::Decimal(value) => {
            Decimal::parse(value)
                .map(RuntimeValue::Decimal)
                .ok_or_else(|| RuleError::InvalidLiteral {
                    rule: rule_name.to_owned(),
                    expected: "decimal".to_owned(),
                })
        }
    }
}

fn evaluate_call(
    rule_name: &str,
    function: Function,
    arguments: &[Expr],
    context: &BTreeMap<String, RuntimeValue>,
) -> Result<RuntimeValue, RuleError> {
    let first = || {
        arguments
            .first()
            .ok_or_else(|| RuleError::InternalInvariant {
                rule: rule_name.to_owned(),
            })
    };
    match function {
        Function::Size => match evaluate_expression(rule_name, first()?, context)? {
            RuntimeValue::String(value) => i32::try_from(value.chars().count())
                .map(RuntimeValue::Int)
                .map_err(|_| arithmetic_overflow(rule_name)),
            RuntimeValue::List(values) => i32::try_from(values.len())
                .map(RuntimeValue::Int)
                .map_err(|_| arithmetic_overflow(rule_name)),
            other => Err(type_mismatch(
                rule_name,
                "string or list",
                other.type_name(),
            )),
        },
        Function::IsNull => Ok(RuntimeValue::Bool(matches!(
            evaluate_expression(rule_name, first()?, context)?,
            RuntimeValue::Null
        ))),
        Function::StartsWith | Function::EndsWith => {
            let left = evaluate_expression(rule_name, first()?, context)?;
            let second = arguments
                .get(1)
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: rule_name.to_owned(),
                })?;
            let right = evaluate_expression(rule_name, second, context)?;
            let (RuntimeValue::String(left), RuntimeValue::String(right)) = (left, right) else {
                return Err(type_mismatch(rule_name, "string", "non-string"));
            };
            Ok(RuntimeValue::Bool(match function {
                Function::StartsWith => left.starts_with(&right),
                Function::EndsWith => left.ends_with(&right),
                _ => {
                    return Err(RuleError::InternalInvariant {
                        rule: rule_name.to_owned(),
                    });
                }
            }))
        }
    }
}

fn evaluate_binary(
    rule_name: &str,
    operation: BinaryOp,
    left: RuntimeValue,
    right: RuntimeValue,
) -> Result<RuntimeValue, RuleError> {
    match operation {
        BinaryOp::Equal => Ok(RuntimeValue::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(RuntimeValue::Bool(!values_equal(&left, &right))),
        BinaryOp::LessThan
        | BinaryOp::LessThanOrEqual
        | BinaryOp::GreaterThan
        | BinaryOp::GreaterThanOrEqual => {
            let ordering = compare_values(rule_name, &left, &right)?;
            let value = match operation {
                BinaryOp::LessThan => ordering.is_lt(),
                BinaryOp::LessThanOrEqual => ordering.is_le(),
                BinaryOp::GreaterThan => ordering.is_gt(),
                BinaryOp::GreaterThanOrEqual => ordering.is_ge(),
                _ => {
                    return Err(RuleError::InternalInvariant {
                        rule: rule_name.to_owned(),
                    });
                }
            };
            Ok(RuntimeValue::Bool(value))
        }
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
            arithmetic_values(rule_name, operation, left, right)
        }
        BinaryOp::And | BinaryOp::Or => Err(RuleError::InternalInvariant {
            rule: rule_name.to_owned(),
        }),
    }
}

fn compare_values(
    rule_name: &str,
    left: &RuntimeValue,
    right: &RuntimeValue,
) -> Result<std::cmp::Ordering, RuleError> {
    match (left, right) {
        (RuntimeValue::Int(left), RuntimeValue::Int(right)) => Ok(left.cmp(right)),
        (RuntimeValue::Int64(left), RuntimeValue::Int64(right)) => Ok(left.cmp(right)),
        (RuntimeValue::Int(left), RuntimeValue::Int64(right)) => Ok(i64::from(*left).cmp(right)),
        (RuntimeValue::Int64(left), RuntimeValue::Int(right)) => Ok(left.cmp(&i64::from(*right))),
        (RuntimeValue::Decimal(left), RuntimeValue::Decimal(right)) => Ok(left.cmp(right)),
        (RuntimeValue::Timestamp(left), RuntimeValue::Timestamp(right)) => {
            compare_canonical_timestamps(left, right).ok_or_else(|| RuleError::InternalInvariant {
                rule: rule_name.to_owned(),
            })
        }
        _ => Err(type_mismatch(
            rule_name,
            "matching numeric or timestamp operands",
            format!("{} and {}", left.type_name(), right.type_name()),
        )),
    }
}

fn values_equal(left: &RuntimeValue, right: &RuntimeValue) -> bool {
    match (left, right) {
        (RuntimeValue::Int(left), RuntimeValue::Int64(right)) => i64::from(*left) == *right,
        (RuntimeValue::Int64(left), RuntimeValue::Int(right)) => *left == i64::from(*right),
        _ => left == right,
    }
}

fn compare_canonical_timestamps(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left_prefix = left.as_bytes().get(..19)?;
    let right_prefix = right.as_bytes().get(..19)?;
    match left_prefix.cmp(right_prefix) {
        std::cmp::Ordering::Equal => {}
        ordering => return Some(ordering),
    }

    let left = canonical_timestamp_fraction(left)?;
    let right = canonical_timestamp_fraction(right)?;
    for index in 0..left.len().max(right.len()) {
        match left
            .get(index)
            .copied()
            .unwrap_or(b'0')
            .cmp(&right.get(index).copied().unwrap_or(b'0'))
        {
            std::cmp::Ordering::Equal => {}
            ordering => return Some(ordering),
        }
    }
    Some(std::cmp::Ordering::Equal)
}

fn canonical_timestamp_fraction(value: &str) -> Option<&[u8]> {
    let bytes = value.as_bytes();
    match bytes.get(19) {
        Some(b'Z') if bytes.len() == 20 => Some(&[]),
        Some(b'.') if bytes.last() == Some(&b'Z') => bytes.get(20..bytes.len() - 1),
        _ => None,
    }
}

fn arithmetic_values(
    rule_name: &str,
    operation: BinaryOp,
    left: RuntimeValue,
    right: RuntimeValue,
) -> Result<RuntimeValue, RuleError> {
    match (left, right) {
        (RuntimeValue::Int(left), RuntimeValue::Int(right)) => {
            let value = match operation {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Subtract => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide => {
                    if right == 0 {
                        return Err(RuleError::DivisionByZero {
                            rule: rule_name.to_owned(),
                        });
                    }
                    left.checked_div(right)
                }
                _ => {
                    return Err(RuleError::InternalInvariant {
                        rule: rule_name.to_owned(),
                    });
                }
            };
            value
                .map(RuntimeValue::Int)
                .ok_or_else(|| arithmetic_overflow(rule_name))
        }
        (RuntimeValue::Int64(left), RuntimeValue::Int64(right)) => {
            checked_i64_arithmetic(rule_name, operation, left, right).map(RuntimeValue::Int64)
        }
        (RuntimeValue::Int(left), RuntimeValue::Int64(right)) => {
            checked_i64_arithmetic(rule_name, operation, i64::from(left), right)
                .map(RuntimeValue::Int64)
        }
        (RuntimeValue::Int64(left), RuntimeValue::Int(right)) => {
            checked_i64_arithmetic(rule_name, operation, left, i64::from(right))
                .map(RuntimeValue::Int64)
        }
        (RuntimeValue::Decimal(left), RuntimeValue::Decimal(right)) => {
            if matches!(operation, BinaryOp::Divide) && right.is_zero() {
                return Err(RuleError::DivisionByZero {
                    rule: rule_name.to_owned(),
                });
            }
            let value = match operation {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Subtract => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide => left.checked_div(right),
                _ => {
                    return Err(RuleError::InternalInvariant {
                        rule: rule_name.to_owned(),
                    });
                }
            };
            value
                .map(RuntimeValue::Decimal)
                .ok_or_else(|| arithmetic_overflow(rule_name))
        }
        (left, right) => Err(type_mismatch(
            rule_name,
            "matching numeric operands",
            format!("{} and {}", left.type_name(), right.type_name()),
        )),
    }
}

fn checked_i64_arithmetic(
    rule_name: &str,
    operation: BinaryOp,
    left: i64,
    right: i64,
) -> Result<i64, RuleError> {
    let value = match operation {
        BinaryOp::Add => left.checked_add(right),
        BinaryOp::Subtract => left.checked_sub(right),
        BinaryOp::Multiply => left.checked_mul(right),
        BinaryOp::Divide => {
            if right == 0 {
                return Err(RuleError::DivisionByZero {
                    rule: rule_name.to_owned(),
                });
            }
            left.checked_div(right)
        }
        _ => {
            return Err(RuleError::InternalInvariant {
                rule: rule_name.to_owned(),
            });
        }
    };
    value.ok_or_else(|| arithmetic_overflow(rule_name))
}

fn arithmetic_overflow(rule_name: &str) -> RuleError {
    RuleError::InvalidLiteral {
        rule: rule_name.to_owned(),
        expected: "an arithmetic result in range".to_owned(),
    }
}

#[derive(Clone, PartialEq, Eq)]
enum RuntimeValue {
    Null,
    Bool(bool),
    String(String),
    Int(i32),
    Int64(i64),
    Decimal(Decimal),
    Uuid(String),
    Date(String),
    Timestamp(String),
    Enum { enum_name: String, symbol: String },
    List(Vec<RuntimeValue>),
    Object(BTreeMap<String, RuntimeValue>),
    OpaqueJson(Value),
}

impl RuntimeValue {
    fn type_name(&self) -> String {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::String(_) => "string",
            Self::Int(_) => "int",
            Self::Int64(_) => "bigint",
            Self::Decimal(_) => "decimal",
            Self::Uuid(_) => "uuid",
            Self::Date(_) => "date",
            Self::Timestamp(_) => "timestamp",
            Self::Enum { .. } => "enum",
            Self::List(_) => "list",
            Self::Object(_) => "object",
            Self::OpaqueJson(_) => "opaque JSON",
        }
        .to_owned()
    }

    fn canonical_json(&self, rule_name: &str, type_: &RuleType) -> Result<Value, RuleError> {
        let invariant = || RuleError::InternalInvariant {
            rule: rule_name.to_owned(),
        };
        match (self, type_) {
            (Self::Null, RuleType::Nullable(_)) => Ok(Value::Null),
            (Self::Bool(value), RuleType::Bool) => Ok(Value::Bool(*value)),
            (Self::String(value), RuleType::String) => Ok(Value::String(value.clone())),
            (Self::Int(value), RuleType::Int) => Ok(Value::Number((*value).into())),
            (Self::Int64(value), RuleType::Int64) => Ok(Value::Number((*value).into())),
            (Self::Int(value), RuleType::Int64) => Ok(Value::Number(i64::from(*value).into())),
            (Self::Decimal(value), RuleType::Decimal) => {
                serde_json::from_str(&value.canonical_string()).map_err(|_| invariant())
            }
            (Self::Uuid(value), RuleType::Uuid)
            | (Self::Date(value), RuleType::Date)
            | (Self::Timestamp(value), RuleType::Timestamp) => Ok(Value::String(value.clone())),
            (Self::Enum { enum_name, symbol }, RuleType::Enum { name, symbols })
                if enum_name == name && symbols.iter().any(|candidate| candidate == symbol) =>
            {
                Ok(Value::String(symbol.clone()))
            }
            (Self::List(values), RuleType::List(item_type)) => values
                .iter()
                .map(|value| value.canonical_json(rule_name, item_type))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            (Self::Object(values), RuleType::Object { fields, .. }) => fields
                .iter()
                .map(|(field, type_)| {
                    values
                        .get(field)
                        .ok_or_else(invariant)
                        .and_then(|value| match value {
                            Self::Null if type_.accepts_null() => {
                                value.canonical_json(rule_name, type_)
                            }
                            // A missing declared object member is represented as
                            // a total-access null at decode time, but a direct
                            // whole-object result must not weaken `field!` into
                            // a JSON null. Member access has already consumed
                            // this value before canonical result validation.
                            Self::Null => Err(RuleError::InvalidRuleResult {
                                rule: rule_name.to_owned(),
                                expected: type_.display_name(),
                                actual: "null".to_owned(),
                            }),
                            value => value.canonical_json(rule_name, type_),
                        })
                        .map(|value| (field.clone(), value))
                })
                .collect::<Result<serde_json::Map<_, _>, _>>()
                .map(Value::Object),
            (Self::OpaqueJson(value), RuleType::OpaqueJson { .. }) => Ok(value.clone()),
            (_, RuleType::Nullable(inner)) => self.canonical_json(rule_name, inner),
            _ => Err(invariant()),
        }
    }
}

/// A bounded decimal representation used only for the profile's exact numeric
/// operations. It deliberately never round-trips through binary floating
/// point, avoiding an implicit CEL `double` conversion.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Decimal {
    coefficient: i128,
    scale: u32,
}

impl Decimal {
    fn parse(source: &str) -> Option<Self> {
        let (negative, unsigned) = match source.strip_prefix('-') {
            Some(unsigned) => (true, unsigned),
            None => (false, source),
        };
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let scale = u32::try_from(fraction.len()).ok()?;
        let digits = format!("{whole}{fraction}");
        let coefficient = if negative {
            format!("-{digits}").parse::<i128>().ok()?
        } else {
            digits.parse::<i128>().ok()?
        };
        Some(Self { coefficient, scale }.normalized())
    }

    fn normalized(mut self) -> Self {
        while self.scale > 0 && self.coefficient % 10 == 0 {
            self.coefficient /= 10;
            self.scale -= 1;
        }
        self
    }

    fn checked_neg(self) -> Option<Self> {
        self.coefficient.checked_neg().map(|coefficient| Self {
            coefficient,
            scale: self.scale,
        })
    }

    fn is_zero(self) -> bool {
        self.coefficient == 0
    }

    fn canonical_string(self) -> String {
        if self.scale == 0 {
            return self.coefficient.to_string();
        }

        let sign = if self.coefficient.is_negative() {
            "-"
        } else {
            ""
        };
        let digits = self.coefficient.unsigned_abs().to_string();
        if digits.len() <= self.scale as usize {
            format!(
                "{sign}0.{}{}",
                "0".repeat(self.scale as usize - digits.len()),
                digits
            )
        } else {
            let split = digits.len() - self.scale as usize;
            format!("{sign}{}.{}", &digits[..split], &digits[split..])
        }
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = self
            .coefficient
            .checked_mul(power_of_ten(scale - self.scale)?)?;
        let right = other
            .coefficient
            .checked_mul(power_of_ten(scale - other.scale)?)?;
        left.checked_add(right)
            .map(|coefficient| Self { coefficient, scale }.normalized())
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        self.checked_add(other.checked_neg()?)
    }

    fn checked_mul(self, other: Self) -> Option<Self> {
        self.coefficient
            .checked_mul(other.coefficient)
            .and_then(|coefficient| {
                self.scale
                    .checked_add(other.scale)
                    .map(|scale| Self { coefficient, scale })
            })
            .map(Self::normalized)
    }

    fn checked_div(self, other: Self) -> Option<Self> {
        if other.coefficient == 0 {
            return None;
        }
        const MIN_DIVISION_SCALE: u32 = 18;
        let scale = MIN_DIVISION_SCALE.max(self.scale.saturating_sub(other.scale));
        let numerator_scale = scale.checked_add(other.scale)?;
        let denominator_scale = self.scale;
        let numerator = self.coefficient.checked_mul(power_of_ten(
            numerator_scale.checked_sub(denominator_scale)?,
        )?)?;
        Some(
            Self {
                coefficient: numerator.checked_div(other.coefficient)?,
                scale,
            }
            .normalized(),
        )
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let left_sign = self.coefficient.cmp(&0);
        let right_sign = other.coefficient.cmp(&0);
        if left_sign != right_sign || left_sign == Ordering::Equal {
            return left_sign.cmp(&right_sign);
        }

        let ordering = compare_decimal_magnitudes(
            self.coefficient.unsigned_abs(),
            self.scale,
            other.coefficient.unsigned_abs(),
            other.scale,
        );
        if left_sign == Ordering::Less {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

fn compare_decimal_magnitudes(
    left: u128,
    left_scale: u32,
    right: u128,
    right_scale: u32,
) -> std::cmp::Ordering {
    let left_digits = left.ilog10() + 1;
    let right_digits = right.ilog10() + 1;
    let left_position = i64::from(left_digits) - i64::from(left_scale);
    let right_position = i64::from(right_digits) - i64::from(right_scale);
    let position_ordering = left_position.cmp(&right_position);
    if position_ordering != std::cmp::Ordering::Equal {
        return position_ordering;
    }

    let mut left_divisor = 10_u128.pow(left_digits - 1);
    let mut right_divisor = 10_u128.pow(right_digits - 1);
    for _ in 0..left_digits.max(right_digits) {
        let left_digit = left.checked_div(left_divisor).unwrap_or_default() % 10;
        left_divisor /= 10;
        let right_digit = right.checked_div(right_divisor).unwrap_or_default() % 10;
        right_divisor /= 10;
        let digit_ordering = left_digit.cmp(&right_digit);
        if digit_ordering != std::cmp::Ordering::Equal {
            return digit_ordering;
        }
    }
    std::cmp::Ordering::Equal
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Encode a complete profile-v1 record. This deliberately writes the
/// normative byte grammar directly; no serde or Rust map serialization is part
/// of the artifact format.
pub fn canonical_bytes(root: CanonicalRoot, value: &CanonicalValue) -> Vec<u8> {
    let mut output = Vec::with_capacity(128);
    output.extend_from_slice(&MAGIC);
    output.extend_from_slice(&PROFILE_VERSION.to_be_bytes());
    output.push(root as u8);

    match (root, value) {
        (CanonicalRoot::TypedRuleAst, CanonicalValue::TypedRule(rule)) => {
            encode_rule_record(&mut output, rule);
        }
        (CanonicalRoot::DecisionDefinition, CanonicalValue::DecisionDefinition(table)) => {
            encode_decision_record(&mut output, table);
        }
        (
            CanonicalRoot::DecodedTypedInput,
            CanonicalValue::DecodedTypedInput { types, bindings },
        ) => {
            let decoded = decode_bindings(types, bindings)
                .expect("canonical decoded input must have complete typed bindings");
            encode_runtime_map(&mut output, types, &decoded);
        }
        _ => panic!("canonical root must match its canonical value"),
    }

    output
}

fn canonical_decoded_input_bytes(
    types: &BTreeMap<String, RuleType>,
    decoded_bindings: &BTreeMap<String, RuntimeValue>,
) -> Vec<u8> {
    let mut output = Vec::with_capacity(128);
    output.extend_from_slice(&MAGIC);
    output.extend_from_slice(&PROFILE_VERSION.to_be_bytes());
    output.push(CanonicalRoot::DecodedTypedInput as u8);
    encode_runtime_map(&mut output, types, decoded_bindings);
    output
}

fn canonical_declarations(
    declared_types: &BTreeMap<String, RuleType>,
    declaration_order: Option<&[String]>,
) -> Result<Vec<RuleType>, RuleError> {
    let names = declaration_order
        .map(|names| names.to_vec())
        .unwrap_or_else(|| declared_types.keys().cloned().collect());
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .map(|name| {
            if !seen.insert(name.clone()) {
                return Err(RuleError::InternalInvariant {
                    rule: "catalog".to_owned(),
                });
            }
            declared_types
                .get(&name)
                .cloned()
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: "catalog".to_owned(),
                })
        })
        .collect()
}

fn derive_decision_revisions(catalog: &mut RuleCatalog) {
    for table in catalog.decision_tables.values_mut() {
        let bytes = canonical_bytes(
            CanonicalRoot::DecisionDefinition,
            &CanonicalValue::DecisionDefinition(table.clone()),
        );
        table.revision = DefinitionRevision(sha256_hex(&bytes));
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn encode_rule_record(output: &mut Vec<u8>, rule: &CompiledRule) {
    encode_string(output, &rule.name);
    encode_type_map(output, &rule.bindings);
    encode_type(output, &rule.result);
    let context = CanonicalExpressionContext {
        bindings: &rule.bindings,
        declared_types: &rule.declared_types,
    };
    encode_expression(
        output,
        &rule.expression.expression,
        &context,
        Some(&rule.result),
    );
}

fn encode_decision_record(output: &mut Vec<u8>, table: &CompiledDecisionTable) {
    encode_string(output, &table.name);
    encode_count(output, table.declared_types.len());
    for declaration in &table.declared_types {
        encode_declaration(output, declaration);
    }
    encode_type_map(output, &table.inputs);
    encode_type_map(output, &table.output);
    output.push(match table.hit_policy {
        HitPolicy::First => 0x00,
        HitPolicy::Unique => 0x01,
        HitPolicy::Unsupported(_) => panic!("validated decision hit policy"),
    });
    encode_count(output, table.rows.len());
    let context = CanonicalExpressionContext {
        bindings: &table.inputs,
        declared_types: &table.declared_types,
    };
    for row in &table.rows {
        encode_string(output, &row.id);
        encode_count(output, row.conditions.len());
        let mut conditions = row.conditions.iter().collect::<Vec<_>>();
        conditions.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        for (name, condition) in conditions {
            encode_string(output, name);
            encode_expression(
                output,
                &condition.expression,
                &context,
                Some(&RuleType::Bool),
            );
        }
        let Value::Object(row_output) = &row.output else {
            panic!("validated decision row output is an object");
        };
        let row_output = row_output.clone().into_iter().collect::<BTreeMap<_, _>>();
        encode_json_map(output, &table.output, &row_output);
    }
    encode_count(output, table.test_cases.len());
    for case in &table.test_cases {
        encode_string(output, &case.name);
        encode_json_map(output, &table.inputs, &case.input);
        encode_json_map(output, &table.output, &case.output);
        encode_string(output, &case.matched_row_id);
    }
}

fn encode_declaration(output: &mut Vec<u8>, declaration: &RuleType) {
    match declaration {
        RuleType::Enum { name, symbols } => {
            output.push(0x40);
            encode_string(output, name);
            encode_count(output, symbols.len());
            for symbol in symbols {
                encode_string(output, symbol);
            }
        }
        RuleType::Object { name, fields } => {
            output.push(0x41);
            encode_string(output, name);
            encode_type_map(output, fields);
        }
        RuleType::OpaqueJson {
            name,
            maximum_bytes,
            maximum_depth,
            maximum_nodes,
        } => {
            output.push(0x42);
            encode_string(output, name);
            output.extend_from_slice(&maximum_bytes.to_be_bytes());
            output.extend_from_slice(&maximum_depth.to_be_bytes());
            output.extend_from_slice(&maximum_nodes.to_be_bytes());
        }
        _ => panic!("resolved declarations are named declaration types"),
    }
}

fn encode_type_map(output: &mut Vec<u8>, types: &BTreeMap<String, RuleType>) {
    encode_count(output, types.len());
    let mut entries = types.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
    for (name, type_) in entries {
        encode_string(output, name);
        encode_type(output, type_);
    }
}

fn encode_type(output: &mut Vec<u8>, type_: &RuleType) {
    match type_ {
        RuleType::Bool => output.push(0x10),
        RuleType::String => output.push(0x11),
        RuleType::Int => output.push(0x12),
        RuleType::Int64 => output.push(0x1c),
        RuleType::Decimal => output.push(0x13),
        RuleType::Uuid => output.push(0x14),
        RuleType::Date => output.push(0x15),
        RuleType::Timestamp => output.push(0x16),
        RuleType::Enum { name, symbols } => {
            output.push(0x17);
            encode_string(output, name);
            encode_count(output, symbols.len());
            for symbol in symbols {
                encode_string(output, symbol);
            }
        }
        RuleType::List(item) => {
            output.push(0x18);
            encode_type(output, item);
        }
        RuleType::Object { name, fields } => {
            output.push(0x19);
            encode_string(output, name);
            encode_type_map(output, fields);
        }
        RuleType::OpaqueJson {
            name,
            maximum_bytes,
            maximum_depth,
            maximum_nodes,
        } => {
            output.push(0x1b);
            encode_string(output, name);
            output.extend_from_slice(&maximum_bytes.to_be_bytes());
            output.extend_from_slice(&maximum_depth.to_be_bytes());
            output.extend_from_slice(&maximum_nodes.to_be_bytes());
        }
        RuleType::Nullable(inner) => {
            output.push(0x1a);
            let mut inner = inner.as_ref();
            while let RuleType::Nullable(next) = inner {
                inner = next;
            }
            encode_type(output, inner);
        }
    }
}

struct CanonicalExpressionContext<'a> {
    bindings: &'a BTreeMap<String, RuleType>,
    declared_types: &'a [RuleType],
}

fn encode_expression(
    output: &mut Vec<u8>,
    expression: &Expr,
    context: &CanonicalExpressionContext<'_>,
    expected: Option<&RuleType>,
) -> RuleType {
    let type_ = expression_type(expression, context, expected);
    match &expression.kind {
        ExprKind::Literal(literal) => {
            output.push(0x20);
            encode_literal(output, literal);
        }
        ExprKind::Name(name) => {
            output.push(0x21);
            encode_string(output, name);
        }
        ExprKind::EnumSymbol { enum_name, symbol } => {
            output.push(0x22);
            encode_string(output, enum_name);
            encode_string(output, symbol);
        }
        ExprKind::List(items) => {
            output.push(0x23);
            encode_count(output, items.len());
            let RuleType::List(item_type) = &type_ else {
                panic!("validated list expression has a list result");
            };
            for item in items {
                encode_expression(output, item, context, Some(item_type));
            }
        }
        ExprKind::Member { target, field } => {
            output.push(0x24);
            encode_expression(output, target, context, None);
            encode_string(output, field);
        }
        ExprKind::Index { target, index } => {
            output.push(0x25);
            encode_expression(output, target, context, None);
            encode_count(output, *index);
        }
        ExprKind::Call {
            function,
            arguments,
        } => {
            output.push(0x26);
            output.push(function_tag(*function));
            encode_count(output, arguments.len());
            for argument in arguments {
                encode_expression(output, argument, context, None);
            }
        }
        ExprKind::Unary { op, operand } => {
            output.push(0x27);
            output.push(unary_tag(*op));
            encode_expression(output, operand, context, None);
        }
        ExprKind::Binary { op, left, right } => {
            output.push(0x28);
            output.push(binary_tag(*op));
            let (left_expected, right_expected) = equality_expected_types(left, right, context);
            encode_expression(output, left, context, left_expected.as_ref());
            encode_expression(output, right, context, right_expected.as_ref());
        }
        ExprKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            output.push(0x29);
            encode_expression(output, condition, context, Some(&RuleType::Bool));
            encode_expression(output, when_true, context, Some(&type_));
            encode_expression(output, when_false, context, Some(&type_));
        }
    }
    encode_type(output, &type_);
    type_
}

fn expression_type(
    expression: &Expr,
    context: &CanonicalExpressionContext<'_>,
    expected: Option<&RuleType>,
) -> RuleType {
    match &expression.kind {
        ExprKind::Literal(Literal::Null) => expected
            .filter(|type_| type_.accepts_null())
            .cloned()
            .expect("validated null expression has a nullable resolved type"),
        ExprKind::Literal(Literal::Bool(_)) => RuleType::Bool,
        ExprKind::Literal(Literal::String(_)) => RuleType::String,
        ExprKind::Literal(Literal::Int(value)) => {
            if matches!(expected, Some(RuleType::Int64)) || value.parse::<i32>().is_err() {
                RuleType::Int64
            } else {
                RuleType::Int
            }
        }
        ExprKind::Literal(Literal::Decimal(_)) => RuleType::Decimal,
        ExprKind::Name(name) => context
            .bindings
            .get(name)
            .cloned()
            .expect("validated expression name"),
        ExprKind::EnumSymbol { enum_name, .. } => context
            .declared_types
            .iter()
            .find(|type_| matches!(type_, RuleType::Enum { name, .. } if name == enum_name))
            .cloned()
            .expect("validated enum symbol"),
        ExprKind::List(items) => {
            let item_expected = match expected {
                Some(RuleType::List(item)) => Some(item.as_ref()),
                _ => None,
            };
            let item = items
                .first()
                .map(|item| expression_type(item, context, item_expected))
                .expect("validated list has an item");
            RuleType::List(Box::new(item))
        }
        ExprKind::Member { target, field } => {
            let RuleType::Object { fields, .. } = expression_type(target, context, None) else {
                panic!("validated member target is an object");
            };
            RuleType::nullable(fields.get(field).cloned().expect("validated object field"))
        }
        ExprKind::Index { target, .. } => {
            let RuleType::List(item) = expression_type(target, context, None) else {
                panic!("validated index target is a list");
            };
            RuleType::nullable(*item)
        }
        ExprKind::Call { function, .. } => match function {
            Function::Size => RuleType::Int,
            Function::IsNull | Function::StartsWith | Function::EndsWith => RuleType::Bool,
        },
        ExprKind::Unary { op, operand } => match op {
            UnaryOp::Not => RuleType::Bool,
            UnaryOp::Negate => expression_type(operand, context, None),
        },
        ExprKind::Binary {
            op, left, right, ..
        } => match op {
            BinaryOp::Or
            | BinaryOp::And
            | BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::LessThan
            | BinaryOp::LessThanOrEqual
            | BinaryOp::GreaterThan
            | BinaryOp::GreaterThanOrEqual => RuleType::Bool,
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                let left = expression_type(left, context, None);
                let right = expression_type(right, context, None);
                if is_mixed_integer_pair(&left, &right) {
                    RuleType::Int64
                } else {
                    left
                }
            }
        },
        ExprKind::Conditional {
            when_true,
            when_false,
            ..
        } => {
            if let Some(expected) = expected {
                return expected.clone();
            }
            match &when_true.kind {
                ExprKind::Literal(Literal::Null) => expression_type(when_false, context, None),
                _ => expression_type(when_true, context, None),
            }
        }
    }
}

fn equality_expected_types(
    left: &Expr,
    right: &Expr,
    context: &CanonicalExpressionContext<'_>,
) -> (Option<RuleType>, Option<RuleType>) {
    if !matches!(left.kind, ExprKind::Literal(Literal::Null))
        && !matches!(right.kind, ExprKind::Literal(Literal::Null))
    {
        return (None, None);
    }
    if matches!(left.kind, ExprKind::Literal(Literal::Null)) {
        let type_ = expression_type(right, context, None);
        return (Some(type_.clone()), Some(type_));
    }
    let type_ = expression_type(left, context, None);
    (Some(type_.clone()), Some(type_))
}

fn encode_literal(output: &mut Vec<u8>, literal: &Literal) {
    match literal {
        Literal::Null => output.push(0x00),
        Literal::Bool(value) => {
            output.push(0x01);
            encode_bool(output, *value);
        }
        Literal::Int(value) => {
            output.push(0x02);
            let value = value
                .parse::<i128>()
                .expect("validated canonical integer literal")
                .to_string();
            encode_string(output, &value);
        }
        Literal::Decimal(value) => {
            output.push(0x03);
            encode_decimal(
                output,
                Decimal::parse(value).expect("validated canonical decimal literal"),
            );
        }
        Literal::String(value) => {
            output.push(0x04);
            encode_string(output, value);
        }
    }
}

fn function_tag(function: Function) -> u8 {
    match function {
        Function::Size => 0x00,
        Function::IsNull => 0x01,
        Function::StartsWith => 0x02,
        Function::EndsWith => 0x03,
    }
}

fn unary_tag(op: UnaryOp) -> u8 {
    match op {
        UnaryOp::Not => 0x00,
        UnaryOp::Negate => 0x01,
    }
}

fn binary_tag(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Or => 0x00,
        BinaryOp::And => 0x01,
        BinaryOp::Equal => 0x02,
        BinaryOp::NotEqual => 0x03,
        BinaryOp::LessThan => 0x04,
        BinaryOp::LessThanOrEqual => 0x05,
        BinaryOp::GreaterThan => 0x06,
        BinaryOp::GreaterThanOrEqual => 0x07,
        BinaryOp::Add => 0x08,
        BinaryOp::Subtract => 0x09,
        BinaryOp::Multiply => 0x0a,
        BinaryOp::Divide => 0x0b,
    }
}

fn encode_json_map(
    output: &mut Vec<u8>,
    types: &BTreeMap<String, RuleType>,
    values: &BTreeMap<String, Value>,
) {
    encode_count(output, types.len());
    let mut entries = types.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
    for (name, type_) in entries {
        let value = values
            .get(name)
            .and_then(|value| decode_value(value, type_))
            .expect("validated canonical typed value");
        encode_string(output, name);
        encode_typed_value(output, type_, &value);
    }
}

fn encode_runtime_map(
    output: &mut Vec<u8>,
    types: &BTreeMap<String, RuleType>,
    values: &BTreeMap<String, RuntimeValue>,
) {
    encode_count(output, types.len());
    let mut entries = types.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
    for (name, type_) in entries {
        encode_string(output, name);
        let value = values
            .get(name)
            .expect("complete decoded canonical bindings");
        // Total object access represents an omitted member as null even when
        // its declared member type is non-null. Canonical values preserve the
        // null only with an explicit nullable value type, as required by the
        // profile grammar.
        if matches!(value, RuntimeValue::Null) && !type_.accepts_null() {
            encode_typed_value(output, &RuleType::nullable(type_.clone()), value);
        } else {
            encode_typed_value(output, type_, value);
        }
    }
}

fn encode_typed_value(output: &mut Vec<u8>, type_: &RuleType, value: &RuntimeValue) {
    encode_type(output, type_);
    match value {
        RuntimeValue::Null => {
            assert!(
                type_.accepts_null(),
                "null requires a nullable canonical type"
            );
            output.push(0x30);
        }
        RuntimeValue::Bool(value) => {
            output.push(0x31);
            encode_bool(output, *value);
        }
        RuntimeValue::String(value) => {
            output.push(0x32);
            encode_string(output, value);
        }
        RuntimeValue::Int(value) => {
            output.push(0x33);
            encode_string(output, &value.to_string());
        }
        RuntimeValue::Int64(value) => {
            output.push(0x3c);
            encode_string(output, &value.to_string());
        }
        RuntimeValue::Decimal(value) => {
            output.push(0x34);
            encode_decimal(output, *value);
        }
        RuntimeValue::Uuid(value) => {
            output.push(0x35);
            encode_string(output, &value.to_ascii_lowercase());
        }
        RuntimeValue::Date(value) => {
            output.push(0x36);
            encode_string(output, value);
        }
        RuntimeValue::Timestamp(value) => {
            output.push(0x37);
            encode_string(output, value);
        }
        RuntimeValue::Enum { enum_name, symbol } => {
            output.push(0x38);
            encode_string(output, enum_name);
            encode_string(output, symbol);
        }
        RuntimeValue::List(values) => {
            output.push(0x39);
            let RuleType::List(item_type) = type_ else {
                panic!("validated list value has a list type");
            };
            encode_count(output, values.len());
            for value in values {
                encode_typed_value(output, item_type, value);
            }
        }
        RuntimeValue::Object(values) => {
            output.push(0x3a);
            let RuleType::Object { fields, .. } = type_ else {
                panic!("validated object value has an object type");
            };
            encode_runtime_map(output, fields, values);
        }
        RuntimeValue::OpaqueJson(value) => {
            output.push(0x3b);
            encode_opaque_json(output, value);
        }
    }
}

fn encode_opaque_json(output: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => output.push(0x00),
        Value::Bool(value) => {
            output.push(0x01);
            encode_bool(output, *value);
        }
        Value::Number(value) => {
            output.push(0x02);
            encode_string(output, &value.to_string());
        }
        Value::String(value) => {
            output.push(0x03);
            encode_string(output, value);
        }
        Value::Array(items) => {
            output.push(0x04);
            encode_count(output, items.len());
            for item in items {
                encode_opaque_json(output, item);
            }
        }
        Value::Object(fields) => {
            output.push(0x05);
            encode_count(output, fields.len());
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (name, value) in fields {
                encode_string(output, name);
                encode_opaque_json(output, value);
            }
        }
    }
}

fn encode_decimal(output: &mut Vec<u8>, value: Decimal) {
    let value = value.normalized();
    if value.coefficient == 0 {
        output.push(0x00);
        encode_string(output, "0");
        output.extend_from_slice(&0_u32.to_be_bytes());
        return;
    }
    output.push(if value.coefficient.is_negative() {
        0x01
    } else {
        0x00
    });
    encode_string(output, &value.coefficient.unsigned_abs().to_string());
    output.extend_from_slice(&value.scale.to_be_bytes());
}

fn encode_bool(output: &mut Vec<u8>, value: bool) {
    output.push(u8::from(value));
}

fn encode_string(output: &mut Vec<u8>, value: &str) {
    encode_count(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn encode_count(output: &mut Vec<u8>, count: usize) {
    let count = u32::try_from(count).expect("canonical profile count fits U32");
    output.extend_from_slice(&count.to_be_bytes());
}

fn power_of_ten(exponent: u32) -> Option<i128> {
    10_i128.checked_pow(exponent)
}
