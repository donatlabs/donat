use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    BinaryOp, CompiledRule, Expr, ExprKind, Function, Literal, RuleError, RuleType, UnaryOp,
};

/// A closed SQL context for one compiled rule. Values are either safe literals
/// produced by this crate or a typed column reference produced through sqlgen.
/// Rule source and rule names are never SQL input.
#[derive(Debug, Clone, Default)]
pub struct SqlBindings {
    values: BTreeMap<String, SqlBinding>,
}

impl SqlBindings {
    pub fn new(values: impl IntoIterator<Item = (String, SqlBinding)>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&SqlBinding> {
        self.values.get(name)
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        self.values.keys()
    }
}

/// One closed source for a declared SQL binding.
#[derive(Debug, Clone)]
pub enum SqlBinding {
    Expression(SqlExpression),
    Literal(Value),
}

impl SqlBinding {
    pub fn expression(expression: SqlExpression) -> Self {
        Self::Expression(expression)
    }

    pub fn literal(value: Value) -> Self {
        Self::Literal(value)
    }
}

/// A typed SQL expression that can only be constructed as an escaped column
/// reference. The textual fragment is private so callers cannot pass CEL text
/// or arbitrary identifiers through this API.
#[derive(Debug, Clone)]
pub struct SqlExpression {
    sql: String,
    type_: RuleType,
}

impl SqlExpression {
    pub fn column(alias: &str, column: &str, type_: RuleType) -> Self {
        Self {
            sql: donat_sqlgen::rule_qualified_column(alias, column),
            type_,
        }
    }
}

/// Lower a fully type-checked declarative rule to one parenthesized Postgres
/// expression. All identifiers and literals pass through sqlgen helpers; the
/// original CEL-like source is intentionally never rendered.
pub fn lower_postgres(rule: &CompiledRule, bindings: &SqlBindings) -> Result<String, RuleError> {
    for name in bindings.names() {
        if !rule.bindings.contains_key(name) {
            return Err(RuleError::UnexpectedBinding { name: name.clone() });
        }
    }
    for name in rule.bindings.keys() {
        if bindings.get(name).is_none() {
            return Err(RuleError::MissingBinding { name: name.clone() });
        }
    }

    let lowered = lower_expression(rule, bindings, &rule.expression.expression)?;
    if !matches!(lowered.type_, ExpressionType::Concrete(RuleType::Bool)) {
        return Err(RuleError::InvalidRuleResult {
            rule: rule.name.clone(),
            expected: "bool".to_owned(),
            actual: expression_type_name(&lowered.type_),
        });
    }
    Ok(lowered.sql)
}

struct LoweredExpression {
    sql: String,
    type_: ExpressionType,
}

#[derive(Clone)]
enum ExpressionType {
    Concrete(RuleType),
    Null,
}

fn lower_expression(
    rule: &CompiledRule,
    bindings: &SqlBindings,
    expression: &Expr,
) -> Result<LoweredExpression, RuleError> {
    let type_ = infer_expression_type(rule, expression)?;
    let sql = match &expression.kind {
        ExprKind::Literal(literal) => lower_literal(&rule.name, literal)?,
        ExprKind::Name(name) => lower_binding(rule, bindings, name)?,
        // Value lowering is introduced as a dedicated later slice. Task 1
        // keeps enum declarations and type checking deploy-time-only.
        ExprKind::EnumSymbol { .. } => {
            return Err(RuleError::InternalInvariant {
                rule: rule.name.clone(),
            });
        }
        ExprKind::List(items) => {
            let items = items
                .iter()
                .map(|item| lower_expression(rule, bindings, item).map(|lowered| lowered.sql))
                .collect::<Result<Vec<_>, _>>()?;
            format!("jsonb_build_array({})", items.join(", "))
        }
        ExprKind::Member { target, field } => {
            let target = lower_expression(rule, bindings, target)?;
            let target_type = concrete_type(&rule.name, &target.type_)?;
            let RuleType::Object { fields, .. } = strip_nullable(target_type) else {
                return Err(RuleError::InternalInvariant {
                    rule: rule.name.clone(),
                });
            };
            let field_type = fields
                .get(field)
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: rule.name.clone(),
                })?;
            json_access(&target.sql, field, field_type)
        }
        ExprKind::Index { target, index } => {
            let target = lower_expression(rule, bindings, target)?;
            let target_type = concrete_type(&rule.name, &target.type_)?;
            let RuleType::List(item_type) = strip_nullable(target_type) else {
                return Err(RuleError::InternalInvariant {
                    rule: rule.name.clone(),
                });
            };
            json_index(&target.sql, *index, item_type)
        }
        ExprKind::Call {
            function,
            arguments,
        } => lower_call(rule, bindings, *function, arguments)?,
        ExprKind::Unary { op, operand } => {
            let operand = lower_expression(rule, bindings, operand)?;
            match op {
                UnaryOp::Not => format!("(NOT ({}))", operand.sql),
                UnaryOp::Negate => format!("(-({}))", operand.sql),
            }
        }
        ExprKind::Binary { op, left, right } => {
            let left = lower_expression(rule, bindings, left)?;
            let right = lower_expression(rule, bindings, right)?;
            lower_binary(*op, left, right)
        }
        ExprKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            let condition = lower_expression(rule, bindings, condition)?;
            let when_true = lower_expression(rule, bindings, when_true)?;
            let when_false = lower_expression(rule, bindings, when_false)?;
            format!(
                "(CASE WHEN ({}) THEN ({}) ELSE ({}) END)",
                condition.sql, when_true.sql, when_false.sql
            )
        }
    };
    Ok(LoweredExpression { sql, type_ })
}

/// Recover the validated type of an AST node without changing the evaluator's
/// Task 3 internals. `CompiledRule` can only originate from `compile_catalog`,
/// so an impossible shape below is an invariant failure rather than a second
/// runtime type-checking surface.
fn infer_expression_type(
    rule: &CompiledRule,
    expression: &Expr,
) -> Result<ExpressionType, RuleError> {
    let invariant = || RuleError::InternalInvariant {
        rule: rule.name.clone(),
    };
    match &expression.kind {
        ExprKind::Literal(Literal::Null) => Ok(ExpressionType::Null),
        ExprKind::Literal(Literal::Bool(_)) => Ok(ExpressionType::Concrete(RuleType::Bool)),
        ExprKind::Literal(Literal::String(_)) => Ok(ExpressionType::Concrete(RuleType::String)),
        ExprKind::Literal(Literal::Int(value)) => value
            .parse::<i128>()
            .map(|_| ExpressionType::Concrete(RuleType::Int))
            .map_err(|_| invariant()),
        ExprKind::Literal(Literal::Decimal(value)) if is_decimal(value) => {
            Ok(ExpressionType::Concrete(RuleType::Decimal))
        }
        ExprKind::Literal(Literal::Decimal(_)) => Err(invariant()),
        ExprKind::Name(name) => rule
            .bindings
            .get(name)
            .cloned()
            .map(ExpressionType::Concrete)
            .ok_or_else(invariant),
        ExprKind::EnumSymbol { .. } => Err(invariant()),
        ExprKind::List(items) => {
            let first = items.first().ok_or_else(invariant)?;
            let ExpressionType::Concrete(item_type) = infer_expression_type(rule, first)? else {
                return Err(invariant());
            };
            Ok(ExpressionType::Concrete(RuleType::List(Box::new(
                item_type,
            ))))
        }
        ExprKind::Member { target, field } => {
            let ExpressionType::Concrete(RuleType::Object { fields, .. }) =
                infer_expression_type(rule, target)?
            else {
                return Err(invariant());
            };
            fields
                .get(field)
                .cloned()
                .map(ExpressionType::Concrete)
                .ok_or_else(invariant)
        }
        ExprKind::Index { target, .. } => {
            let ExpressionType::Concrete(RuleType::List(item_type)) =
                infer_expression_type(rule, target)?
            else {
                return Err(invariant());
            };
            Ok(ExpressionType::Concrete(*item_type))
        }
        ExprKind::Call { function, .. } => match function {
            Function::Size => Ok(ExpressionType::Concrete(RuleType::Int)),
            Function::IsNull | Function::StartsWith | Function::EndsWith => {
                Ok(ExpressionType::Concrete(RuleType::Bool))
            }
        },
        ExprKind::Unary {
            op: UnaryOp::Not, ..
        } => Ok(ExpressionType::Concrete(RuleType::Bool)),
        ExprKind::Unary {
            op: UnaryOp::Negate,
            operand,
        } => infer_expression_type(rule, operand),
        ExprKind::Binary {
            op:
                BinaryOp::Or
                | BinaryOp::And
                | BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::LessThan
                | BinaryOp::LessThanOrEqual
                | BinaryOp::GreaterThan
                | BinaryOp::GreaterThanOrEqual,
            ..
        } => Ok(ExpressionType::Concrete(RuleType::Bool)),
        ExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            left,
            ..
        } => infer_expression_type(rule, left),
        ExprKind::Conditional {
            when_true,
            when_false,
            ..
        } => merge_conditional_types(
            &rule.name,
            infer_expression_type(rule, when_true)?,
            infer_expression_type(rule, when_false)?,
        ),
    }
}

fn merge_conditional_types(
    rule_name: &str,
    when_true: ExpressionType,
    when_false: ExpressionType,
) -> Result<ExpressionType, RuleError> {
    match (when_true, when_false) {
        (ExpressionType::Concrete(left), ExpressionType::Concrete(right)) if left == right => {
            Ok(ExpressionType::Concrete(left))
        }
        (ExpressionType::Null, ExpressionType::Concrete(other))
        | (ExpressionType::Concrete(other), ExpressionType::Null)
            if other.accepts_null() =>
        {
            Ok(ExpressionType::Concrete(other))
        }
        _ => Err(RuleError::InternalInvariant {
            rule: rule_name.to_owned(),
        }),
    }
}

fn lower_binding(
    rule: &CompiledRule,
    bindings: &SqlBindings,
    name: &str,
) -> Result<String, RuleError> {
    let expected = rule
        .bindings
        .get(name)
        .ok_or_else(|| RuleError::InternalInvariant {
            rule: rule.name.clone(),
        })?;
    match bindings
        .get(name)
        .ok_or_else(|| RuleError::MissingBinding {
            name: name.to_owned(),
        })? {
        SqlBinding::Expression(expression) if expression.type_ == *expected => {
            Ok(cast_sql_expression(&expression.sql, expected))
        }
        SqlBinding::Expression(expression) => Err(RuleError::TypeMismatch {
            rule: rule.name.clone(),
            expected: expected.display_name(),
            actual: expression.type_.display_name(),
        }),
        SqlBinding::Literal(value) => lower_json_literal(rule, name, expected, value),
    }
}

fn lower_json_literal(
    rule: &CompiledRule,
    name: &str,
    type_: &RuleType,
    value: &Value,
) -> Result<String, RuleError> {
    lower_value_literal(&rule.name, name, type_, value)
}

fn lower_value_literal(
    rule_name: &str,
    binding_name: &str,
    type_: &RuleType,
    value: &Value,
) -> Result<String, RuleError> {
    if let RuleType::Nullable(inner) = type_ {
        return if value.is_null() {
            Ok(format!("NULL::{}", postgres_type(inner)))
        } else {
            lower_value_literal(rule_name, binding_name, inner, value)
        };
    }

    match type_ {
        RuleType::Bool => value
            .as_bool()
            .map(|value| {
                if value {
                    "TRUE".to_owned()
                } else {
                    "FALSE".to_owned()
                }
            })
            .ok_or_else(|| invalid_literal(rule_name, type_)),
        RuleType::String => {
            let value = value
                .as_str()
                .ok_or_else(|| invalid_literal(rule_name, type_))?;
            Ok(format!(
                "{}::{}",
                donat_sqlgen::quote_lit(value),
                postgres_type(type_)
            ))
        }
        RuleType::Uuid => {
            let value = value
                .as_str()
                .filter(|value| is_uuid(value))
                .ok_or_else(|| invalid_binding(binding_name, type_))?;
            Ok(format!("{}::uuid", donat_sqlgen::quote_lit(value)))
        }
        RuleType::Date => {
            let value = value
                .as_str()
                .filter(|value| is_canonical_date(value))
                .ok_or_else(|| invalid_binding(binding_name, type_))?;
            Ok(format!("{}::date", donat_sqlgen::quote_lit(value)))
        }
        RuleType::Timestamp => {
            let value = value
                .as_str()
                .filter(|value| is_canonical_timestamp(value))
                .ok_or_else(|| invalid_binding(binding_name, type_))?;
            Ok(format!("{}::timestamptz", donat_sqlgen::quote_lit(value)))
        }
        RuleType::Enum { symbols, .. } => {
            let value = value
                .as_str()
                .filter(|value| symbols.iter().any(|symbol| symbol == *value))
                .ok_or_else(|| invalid_binding(binding_name, type_))?;
            Ok(format!("{}::text", donat_sqlgen::quote_lit(value)))
        }
        RuleType::Int => {
            let value = value
                .as_number()
                .ok_or_else(|| invalid_literal(rule_name, type_))?;
            value
                .to_string()
                .parse::<i128>()
                .map_err(|_| invalid_binding(binding_name, type_))?;
            Ok(format!("({})::numeric", value))
        }
        RuleType::Decimal => {
            let value = value
                .as_number()
                .ok_or_else(|| invalid_literal(rule_name, type_))?;
            if !is_decimal(&value.to_string()) {
                return Err(invalid_binding(binding_name, type_));
            }
            Ok(format!("({})::numeric", value))
        }
        RuleType::List(item_type) => {
            let items = value
                .as_array()
                .ok_or_else(|| invalid_literal(rule_name, type_))?
                .iter()
                .map(|item| lower_value_literal(rule_name, binding_name, item_type, item))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("jsonb_build_array({})", items.join(", ")))
        }
        RuleType::Object { fields, .. } => {
            let object = value
                .as_object()
                .ok_or_else(|| invalid_literal(rule_name, type_))?;
            let mut pairs = Vec::with_capacity(fields.len() * 2);
            for (field, field_type) in fields {
                let value = object
                    .get(field)
                    .ok_or_else(|| invalid_literal(rule_name, type_))?;
                pairs.push(donat_sqlgen::quote_lit(field));
                pairs.push(lower_value_literal(
                    rule_name,
                    binding_name,
                    field_type,
                    value,
                )?);
            }
            Ok(format!("jsonb_build_object({})", pairs.join(", ")))
        }
        RuleType::Nullable(_) => unreachable!("nullable values are handled above"),
    }
}

fn lower_literal(rule_name: &str, literal: &Literal) -> Result<String, RuleError> {
    match literal {
        Literal::Null => Ok("NULL".to_owned()),
        Literal::Bool(value) => Ok(if *value { "TRUE" } else { "FALSE" }.to_owned()),
        Literal::String(value) => Ok(format!("{}::text", donat_sqlgen::quote_lit(value))),
        Literal::Int(value) => value
            .parse::<i128>()
            .map(|_| format!("({value})::numeric"))
            .map_err(|_| RuleError::InvalidLiteral {
                rule: rule_name.to_owned(),
                expected: "int".to_owned(),
            }),
        Literal::Decimal(value) => Ok(format!("({value})::numeric")),
    }
}

fn lower_call(
    rule: &CompiledRule,
    bindings: &SqlBindings,
    function: Function,
    arguments: &[Expr],
) -> Result<String, RuleError> {
    let first = arguments
        .first()
        .ok_or_else(|| RuleError::InternalInvariant {
            rule: rule.name.clone(),
        })?;
    let first = lower_expression(rule, bindings, first)?;
    match function {
        Function::Size => match concrete_type(&rule.name, &first.type_)? {
            RuleType::String => Ok(format!("char_length(({})::text)", first.sql)),
            RuleType::List(_) => Ok(format!("jsonb_array_length(({})::jsonb)", first.sql)),
            _ => Err(RuleError::InternalInvariant {
                rule: rule.name.clone(),
            }),
        },
        Function::IsNull => Ok(format!("(({}) IS NULL)", first.sql)),
        Function::StartsWith | Function::EndsWith => {
            let second = arguments
                .get(1)
                .ok_or_else(|| RuleError::InternalInvariant {
                    rule: rule.name.clone(),
                })?;
            let second = lower_expression(rule, bindings, second)?;
            let function = match function {
                Function::StartsWith => "left",
                Function::EndsWith => "right",
                _ => unreachable!("the outer match restricts the function"),
            };
            Ok(format!(
                "(({}(({})::text, char_length(({})::text))) = (({})::text))",
                function, first.sql, second.sql, second.sql
            ))
        }
    }
}

fn lower_binary(operation: BinaryOp, left: LoweredExpression, right: LoweredExpression) -> String {
    match operation {
        BinaryOp::Or => format!("(({}) OR ({}))", left.sql, right.sql),
        BinaryOp::And => format!("(({}) AND ({}))", left.sql, right.sql),
        BinaryOp::Equal if matches!(right.type_, ExpressionType::Null) => {
            format!("(({}) IS NULL)", left.sql)
        }
        BinaryOp::Equal if matches!(left.type_, ExpressionType::Null) => {
            format!("(({}) IS NULL)", right.sql)
        }
        BinaryOp::NotEqual if matches!(right.type_, ExpressionType::Null) => {
            format!("(({}) IS NOT NULL)", left.sql)
        }
        BinaryOp::NotEqual if matches!(left.type_, ExpressionType::Null) => {
            format!("(({}) IS NOT NULL)", right.sql)
        }
        BinaryOp::Equal => format!("(({}) = ({}))", left.sql, right.sql),
        BinaryOp::NotEqual => format!("(({}) <> ({}))", left.sql, right.sql),
        BinaryOp::LessThan => format!("(({}) < ({}))", left.sql, right.sql),
        BinaryOp::LessThanOrEqual => format!("(({}) <= ({}))", left.sql, right.sql),
        BinaryOp::GreaterThan => format!("(({}) > ({}))", left.sql, right.sql),
        BinaryOp::GreaterThanOrEqual => format!("(({}) >= ({}))", left.sql, right.sql),
        BinaryOp::Add => format!("(({}) + ({}))", left.sql, right.sql),
        BinaryOp::Subtract => format!("(({}) - ({}))", left.sql, right.sql),
        BinaryOp::Multiply => format!("(({}) * ({}))", left.sql, right.sql),
        BinaryOp::Divide if matches!(left.type_, ExpressionType::Concrete(RuleType::Int)) => {
            format!("trunc(({}) / ({}))", left.sql, right.sql)
        }
        BinaryOp::Divide if matches!(left.type_, ExpressionType::Concrete(RuleType::Decimal)) => {
            decimal_divide(&left.sql, &right.sql)
        }
        BinaryOp::Divide => format!("(({}) / ({}))", left.sql, right.sql),
    }
}

fn decimal_divide(left: &str, right: &str) -> String {
    // Decimal::checked_div preserves at least 18 fractional digits and truncates
    // toward zero. `trim_scale` makes PostgreSQL's runtime numeric scale match
    // Decimal::normalized before deriving the target scale.
    let target_scale = format!(
        "greatest(18, scale(trim_scale(({left})::numeric)) - scale(trim_scale(({right})::numeric)))"
    );
    format!("(trim_scale(trunc((({left}) / ({right})), {target_scale})))")
}

fn json_access(target: &str, field: &str, type_: &RuleType) -> String {
    let value = format!(
        "(({})::jsonb -> {})",
        target,
        donat_sqlgen::quote_lit(field)
    );
    json_value(value, type_)
}

fn json_index(target: &str, index: usize, type_: &RuleType) -> String {
    let value = format!("(({})::jsonb -> {index})", target);
    json_value(value, type_)
}

fn json_value(value: String, type_: &RuleType) -> String {
    match strip_nullable(type_) {
        RuleType::List(_) | RuleType::Object { .. } => {
            format!("NULLIF(({value}), 'null'::jsonb)")
        }
        type_ => format!(
            "((NULLIF(({value}), 'null'::jsonb) #>> '{{}}'))::{}",
            postgres_type(type_)
        ),
    }
}

fn cast_sql_expression(expression: &str, type_: &RuleType) -> String {
    format!("({expression})::{}", postgres_type(type_))
}

fn concrete_type<'a>(
    rule_name: &str,
    type_: &'a ExpressionType,
) -> Result<&'a RuleType, RuleError> {
    match type_ {
        ExpressionType::Concrete(type_) => Ok(type_),
        ExpressionType::Null => Err(RuleError::InternalInvariant {
            rule: rule_name.to_owned(),
        }),
    }
}

fn strip_nullable(type_: &RuleType) -> &RuleType {
    match type_ {
        RuleType::Nullable(inner) => strip_nullable(inner),
        type_ => type_,
    }
}

fn postgres_type(type_: &RuleType) -> &'static str {
    match strip_nullable(type_) {
        RuleType::Bool => "boolean",
        RuleType::String | RuleType::Enum { .. } => "text",
        RuleType::Int | RuleType::Decimal => "numeric",
        RuleType::Uuid => "uuid",
        RuleType::Date => "date",
        RuleType::Timestamp => "timestamptz",
        RuleType::List(_) | RuleType::Object { .. } => "jsonb",
        RuleType::Nullable(_) => unreachable!("nullable types are unwrapped above"),
    }
}

fn expression_type_name(type_: &ExpressionType) -> String {
    match type_ {
        ExpressionType::Concrete(type_) => type_.display_name(),
        ExpressionType::Null => "null".to_owned(),
    }
}

fn invalid_literal(rule_name: &str, type_: &RuleType) -> RuleError {
    RuleError::InvalidLiteral {
        rule: rule_name.to_owned(),
        expected: type_.display_name(),
    }
}

fn invalid_binding(name: &str, type_: &RuleType) -> RuleError {
    RuleError::InvalidBinding {
        name: name.to_owned(),
        expected: type_.display_name(),
    }
}

fn is_decimal(source: &str) -> bool {
    let (negative, unsigned) = match source.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, source),
    };
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let digits = format!("{whole}{fraction}");
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    signed.parse::<i128>().is_ok()
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
    let (Some(year), Some(month), Some(day)) = (
        value[0..4].parse::<u32>().ok(),
        value[5..7].parse::<u32>().ok(),
        value[8..10].parse::<u32>().ok(),
    ) else {
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
        value[11..13].parse::<u32>().ok(),
        value[14..16].parse::<u32>().ok(),
        value[17..19].parse::<u32>().ok(),
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
                && value[zone_start + 1..zone_start + 3]
                    .parse::<u32>()
                    .ok()
                    .is_some_and(|offset_hour| offset_hour <= 23)
                && value[zone_start + 4..zone_start + 6]
                    .parse::<u32>()
                    .ok()
                    .is_some_and(|offset_minute| offset_minute <= 59)
        }
        _ => false,
    }
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
