use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::types::{CheckedExpr, CheckedType, CompiledDecisionRow, CompiledDecisionTable};
use crate::{
    BinaryOp, CompiledRule, DecisionConditionTrace, DecisionRejection, DecisionResult,
    DecisionTableDefinition, DecisionTrace, Expr, ExprKind, Function, HitPolicy, Literal,
    RuleCatalog, RuleDefinition, RuleError, RuleType, UnaryOp, parse_expression,
};

pub type RuleBindings = BTreeMap<String, Value>;

/// Type-check a closed catalog at deploy time. Evaluation only receives this
/// compiled representation and explicitly supplied JSON bindings.
pub fn compile_catalog(
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
) -> Result<RuleCatalog, RuleError> {
    let mut catalog = RuleCatalog::default();

    for definition in rules {
        if catalog.rules.contains_key(&definition.name)
            || catalog.decision_tables.contains_key(&definition.name)
        {
            return Err(RuleError::DuplicateName {
                kind: "rule",
                name: definition.name.clone(),
            });
        }
        let expression = parse_expression(&definition.name, &definition.expression)?;
        let checked = check_expression(&definition.name, &expression, &definition.bindings)?;
        if !is_assignable(&checked.type_, &definition.result) {
            return Err(RuleError::InvalidRuleResult {
                rule: definition.name.clone(),
                expected: definition.result.display_name(),
                actual: checked_type_name(&checked.type_),
            });
        }
        catalog.rules.insert(
            definition.name.clone(),
            CompiledRule {
                name: definition.name.clone(),
                bindings: definition.bindings.clone(),
                result: definition.result.clone(),
                expression: checked,
            },
        );
    }

    for definition in tables {
        if catalog.rules.contains_key(&definition.name)
            || catalog.decision_tables.contains_key(&definition.name)
        {
            return Err(RuleError::DuplicateName {
                kind: "decision table",
                name: definition.name.clone(),
            });
        }
        let compiled = compile_decision_table(definition)?;
        catalog
            .decision_tables
            .insert(definition.name.clone(), compiled);
    }

    validate_decision_test_cases(&catalog, tables)?;
    Ok(catalog)
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

/// Evaluate a compiled boolean rule over its complete, closed bindings.
pub fn evaluate_bool(rule: &CompiledRule, bindings: &RuleBindings) -> Result<bool, RuleError> {
    let context = decode_bindings(&rule.bindings, bindings)?;
    let value = evaluate_expression(&rule.name, &rule.expression.expression, &context)?;
    match value {
        RuntimeValue::Bool(value) => Ok(value),
        other => Err(RuleError::InvalidRuleResult {
            rule: rule.name.clone(),
            expected: "bool".to_owned(),
            actual: other.type_name(),
        }),
    }
}

fn compile_decision_table(
    definition: &DecisionTableDefinition,
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
    for field in definition.output.keys() {
        if is_permission_selecting_output(field) {
            return Err(RuleError::ForbiddenDecisionOutput {
                field: field.clone(),
            });
        }
    }

    let mut seen_rows = BTreeSet::new();
    let mut rows = Vec::with_capacity(definition.rows.len());
    for row in &definition.rows {
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
            let expression = parse_expression(&definition.name, source)?;
            let checked = check_expression(&definition.name, &expression, &definition.inputs)?;
            if !matches!(checked.type_, CheckedType::Concrete(RuleType::Bool)) {
                return Err(RuleError::InvalidDecisionRow {
                    table: definition.name.clone(),
                    row_id: row.id.clone(),
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

    Ok(CompiledDecisionTable {
        name: definition.name.clone(),
        revision: definition.revision.clone(),
        inputs: definition.inputs.clone(),
        hit_policy: policy,
        rows,
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
    rule_name: &str,
    expression: &Expr,
    bindings: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError> {
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
        ExprKind::List(items) => check_list(rule_name, items, bindings)?,
        ExprKind::Member { target, field } => {
            let target = check_expression(rule_name, target, bindings)?;
            let object = required_concrete(rule_name, &target.type_)?;
            match object {
                RuleType::Object(fields) => {
                    CheckedType::Concrete(fields.get(field).cloned().ok_or_else(|| {
                        RuleError::UnknownField {
                            rule: rule_name.to_owned(),
                            field: field.clone(),
                        }
                    })?)
                }
                other => return Err(type_mismatch(rule_name, "object", other.display_name())),
            }
        }
        ExprKind::Index { target, index: _ } => {
            let target = check_expression(rule_name, target, bindings)?;
            let list = required_concrete(rule_name, &target.type_)?;
            match list {
                RuleType::List(item) => CheckedType::Concrete((**item).clone()),
                other => return Err(type_mismatch(rule_name, "list", other.display_name())),
            }
        }
        ExprKind::Call {
            function,
            arguments,
        } => check_call(rule_name, *function, arguments, bindings)?,
        ExprKind::Unary { op, operand } => {
            let operand = check_expression(rule_name, operand, bindings)?;
            let operand = required_concrete(rule_name, &operand.type_)?;
            match op {
                UnaryOp::Not if matches!(operand, RuleType::Bool) => {
                    CheckedType::Concrete(RuleType::Bool)
                }
                UnaryOp::Negate if is_numeric(operand) => CheckedType::Concrete(operand.clone()),
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
            let left = check_expression(rule_name, left, bindings)?;
            let right = check_expression(rule_name, right, bindings)?;
            check_binary(rule_name, *op, &left.type_, &right.type_)?
        }
        ExprKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            let condition = check_expression(rule_name, condition, bindings)?;
            expect_bool(rule_name, &condition.type_)?;
            let when_true = check_expression(rule_name, when_true, bindings)?;
            let when_false = check_expression(rule_name, when_false, bindings)?;
            merge_branch_types(rule_name, &when_true.type_, &when_false.type_)?
        }
    };
    Ok(CheckedExpr {
        expression: expression.clone(),
        type_,
    })
}

fn check_literal(rule_name: &str, literal: &Literal) -> Result<CheckedType, RuleError> {
    match literal {
        Literal::Null => Ok(CheckedType::Null),
        Literal::Bool(_) => Ok(CheckedType::Concrete(RuleType::Bool)),
        Literal::String(_) => Ok(CheckedType::Concrete(RuleType::String)),
        Literal::Int(value) => value
            .parse::<i128>()
            .map(|_| CheckedType::Concrete(RuleType::Int))
            .map_err(|_| RuleError::InvalidLiteral {
                rule: rule_name.to_owned(),
                expected: "int".to_owned(),
            }),
        Literal::Decimal(value) => Decimal::parse(value)
            .map(|_| CheckedType::Concrete(RuleType::Decimal))
            .ok_or_else(|| RuleError::InvalidLiteral {
                rule: rule_name.to_owned(),
                expected: "decimal".to_owned(),
            }),
    }
}

fn check_list(
    rule_name: &str,
    items: &[Expr],
    bindings: &BTreeMap<String, RuleType>,
) -> Result<CheckedType, RuleError> {
    let Some((first, rest)) = items.split_first() else {
        return Err(RuleError::InvalidLiteral {
            rule: rule_name.to_owned(),
            expected: "a non-empty typed list".to_owned(),
        });
    };
    let first = check_expression(rule_name, first, bindings)?;
    let CheckedType::Concrete(item_type) = first.type_ else {
        return Err(RuleError::InvalidLiteral {
            rule: rule_name.to_owned(),
            expected: "a non-null typed list item".to_owned(),
        });
    };
    for item in rest {
        let item = check_expression(rule_name, item, bindings)?;
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
    rule_name: &str,
    function: Function,
    arguments: &[Expr],
    bindings: &BTreeMap<String, RuleType>,
) -> Result<CheckedType, RuleError> {
    let checked = arguments
        .iter()
        .map(|argument| check_expression(rule_name, argument, bindings))
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
            if left == right && is_numeric(left) {
                Ok(CheckedType::Concrete(RuleType::Bool))
            } else {
                Err(type_mismatch(
                    rule_name,
                    "matching int or decimal operands",
                    format!("{} and {}", left.display_name(), right.display_name()),
                ))
            }
        }
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
            let left = required_concrete(rule_name, left)?;
            let right = required_concrete(rule_name, right)?;
            if left == right && is_numeric(left) {
                Ok(CheckedType::Concrete(left.clone()))
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
        (CheckedType::Concrete(left), CheckedType::Concrete(right)) if left == right => Ok(()),
        _ => Err(type_mismatch(
            rule_name,
            checked_type_name(left),
            checked_type_name(right),
        )),
    }
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
        CheckedType::Concrete(actual) => actual == expected,
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
    matches!(type_, RuleType::Int | RuleType::Decimal)
}

fn is_permission_selecting_output(field: &str) -> bool {
    let normalized = field.to_ascii_lowercase();
    normalized.contains("role") || normalized.contains("permission")
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
                    bindings,
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
                    bindings,
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
                    bindings,
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
            bindings,
        ),
    })
}

fn decision_trace(
    table: &CompiledDecisionTable,
    matched_row_id: Option<String>,
    rejection: Option<DecisionRejection>,
    condition_results: Vec<DecisionConditionTrace>,
    bindings: &RuleBindings,
) -> DecisionTrace {
    DecisionTrace {
        table_name: table.name.clone(),
        table_revision: table.revision.clone(),
        matched_row_id,
        rejection,
        condition_results,
        input_digest: digest_bindings(bindings),
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
            .and_then(|number| number.to_string().parse::<i128>().ok())
            .map(RuntimeValue::Int),
        RuleType::Decimal => value
            .as_number()
            .and_then(|number| Decimal::parse(&number.to_string()))
            .map(RuntimeValue::Decimal),
        RuleType::Uuid => value
            .as_str()
            .filter(|value| is_uuid(value))
            .map(|value| RuntimeValue::Uuid(value.to_owned())),
        RuleType::Date => value
            .as_str()
            .filter(|value| is_canonical_date(value))
            .map(|value| RuntimeValue::Date(value.to_owned())),
        RuleType::Timestamp => value
            .as_str()
            .filter(|value| is_canonical_timestamp(value))
            .map(|value| RuntimeValue::Timestamp(value.to_owned())),
        RuleType::Enum(symbols) => value
            .as_str()
            .filter(|value| symbols.iter().any(|symbol| symbol == value))
            .map(|value| RuntimeValue::Enum(value.to_owned())),
        RuleType::List(item_type) => value.as_array().and_then(|items| {
            items
                .iter()
                .map(|item| decode_value(item, item_type))
                .collect::<Option<Vec<_>>>()
                .map(RuntimeValue::List)
        }),
        RuleType::Object(fields) => value.as_object().and_then(|object| {
            fields
                .iter()
                .map(|(field, type_)| {
                    object
                        .get(field)
                        .and_then(|value| decode_value(value, type_))
                        .map(|value| (field.clone(), value))
                })
                .collect::<Option<BTreeMap<_, _>>>()
                .map(RuntimeValue::Object)
        }),
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
        ExprKind::List(items) => items
            .iter()
            .map(|item| evaluate_expression(rule_name, item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(RuntimeValue::List),
        ExprKind::Member { target, field } => {
            match evaluate_expression(rule_name, target, context)? {
                RuntimeValue::Object(fields) => {
                    fields
                        .get(field)
                        .cloned()
                        .ok_or_else(|| RuleError::UnknownField {
                            rule: rule_name.to_owned(),
                            field: field.clone(),
                        })
                }
                other => Err(type_mismatch(rule_name, "object", other.type_name())),
            }
        }
        ExprKind::Index { target, index } => match evaluate_expression(rule_name, target, context)?
        {
            RuntimeValue::List(items) => {
                items
                    .get(*index)
                    .cloned()
                    .ok_or_else(|| RuleError::InvalidBinding {
                        name: "list index".to_owned(),
                        expected: "an in-range literal list index".to_owned(),
                    })
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
            value
                .parse::<i128>()
                .map(RuntimeValue::Int)
                .map_err(|_| RuleError::InvalidLiteral {
                    rule: rule_name.to_owned(),
                    expected: "int".to_owned(),
                })
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
            RuntimeValue::String(value) => Ok(RuntimeValue::Int(value.chars().count() as i128)),
            RuntimeValue::List(values) => Ok(RuntimeValue::Int(values.len() as i128)),
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
        BinaryOp::Equal => Ok(RuntimeValue::Bool(left == right)),
        BinaryOp::NotEqual => Ok(RuntimeValue::Bool(left != right)),
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
        (RuntimeValue::Decimal(left), RuntimeValue::Decimal(right)) => Ok(left.cmp(right)),
        _ => Err(type_mismatch(
            rule_name,
            "matching numeric operands",
            format!("{} and {}", left.type_name(), right.type_name()),
        )),
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
    Int(i128),
    Decimal(Decimal),
    Uuid(String),
    Date(String),
    Timestamp(String),
    Enum(String),
    List(Vec<RuntimeValue>),
    Object(BTreeMap<String, RuntimeValue>),
}

impl RuntimeValue {
    fn type_name(&self) -> String {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::String(_) => "string",
            Self::Int(_) => "int",
            Self::Decimal(_) => "decimal",
            Self::Uuid(_) => "uuid",
            Self::Date(_) => "date",
            Self::Timestamp(_) => "timestamp",
            Self::Enum(_) => "enum",
            Self::List(_) => "list",
            Self::Object(_) => "object",
        }
        .to_owned()
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

        match (self.coefficient.cmp(&0), other.coefficient.cmp(&0)) {
            (Ordering::Less, Ordering::Greater | Ordering::Equal) => return Ordering::Less,
            (Ordering::Greater | Ordering::Equal, Ordering::Less) => return Ordering::Greater,
            (Ordering::Equal, Ordering::Equal) => return Ordering::Equal,
            _ => {}
        }
        let scale = self.scale.max(other.scale);
        let mut left = self.coefficient.unsigned_abs().to_string();
        let mut right = other.coefficient.unsigned_abs().to_string();
        left.extend(std::iter::repeat_n('0', (scale - self.scale) as usize));
        right.extend(std::iter::repeat_n('0', (scale - other.scale) as usize));
        let ordering = left.len().cmp(&right.len()).then_with(|| left.cmp(&right));
        if self.coefficient.is_negative() {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn power_of_ten(exponent: u32) -> Option<i128> {
    10_i128.checked_pow(exponent)
}

fn digest_bindings(bindings: &RuleBindings) -> String {
    let mut canonical = String::new();
    for (name, value) in bindings {
        canonical.push_str(name);
        canonical.push(':');
        canonical_json(value, &mut canonical);
        canonical.push(';');
    }
    let hash = canonical
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

fn canonical_json(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => {
            output.push('"');
            for character in value.chars() {
                for escaped in character.escape_default() {
                    output.push(escaped);
                }
            }
            output.push('"');
        }
        Value::Array(values) => {
            output.push('[');
            for value in values {
                canonical_json(value, output);
                output.push(',');
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            let mut sorted = values.iter().collect::<Vec<_>>();
            sorted.sort_unstable_by_key(|(key, _)| *key);
            for (key, value) in sorted {
                output.push_str(key);
                output.push(':');
                canonical_json(value, output);
                output.push(',');
            }
            output.push('}');
        }
    }
}
