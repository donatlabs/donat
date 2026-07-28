use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{Expr, Span};

/// Deploy-time metadata location and owner for one source expression. The
/// compiler receives this from the metadata adapter so the rules crate stays
/// independent from YAML parsing while preserving user-actionable locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionContext {
    pub metadata_path: String,
    pub expression_owner: ExpressionOwner,
}

/// The closed set of deploy-time sources that may contain rule expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionOwner {
    Rule {
        name: String,
    },
    DecisionCondition {
        table_name: String,
        row_id: String,
        input_name: String,
    },
}

impl ExpressionContext {
    pub(crate) fn parser_name(&self) -> &str {
        match &self.expression_owner {
            ExpressionOwner::Rule { name } => name,
            ExpressionOwner::DecisionCondition { table_name, .. } => table_name,
        }
    }
}

/// A stable semantic validation location. It retains only deploy-time source
/// ownership, a byte span, and the existing redacted validation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDiagnostic {
    pub context: ExpressionContext,
    pub span: Span,
    pub message: String,
}

/// Types admitted by the restricted declarative rule profile.
///
/// A nullable value has an explicit wrapper. There is no implicit nullable
/// widening: type checking owns every occurrence of [`RuleType::Nullable`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleType {
    Bool,
    String,
    Int,
    Decimal,
    Uuid,
    Date,
    Timestamp,
    Enum {
        name: String,
        symbols: Vec<String>,
    },
    List(Box<RuleType>),
    Object {
        name: String,
        fields: BTreeMap<String, RuleType>,
    },
    Nullable(Box<RuleType>),
}

impl RuleType {
    pub fn nullable(inner: RuleType) -> Self {
        match inner {
            Self::Nullable(_) => inner,
            other => Self::Nullable(Box::new(other)),
        }
    }

    pub(crate) fn accepts_null(&self) -> bool {
        matches!(self, Self::Nullable(_))
    }

    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::Bool => "bool".to_owned(),
            Self::String => "string".to_owned(),
            Self::Int => "int".to_owned(),
            Self::Decimal => "decimal".to_owned(),
            Self::Uuid => "uuid".to_owned(),
            Self::Date => "date".to_owned(),
            Self::Timestamp => "timestamp".to_owned(),
            Self::Enum { name, .. } => format!("enum {name}"),
            Self::List(inner) => format!("list<{}>", inner.display_name()),
            Self::Object { name, .. } => format!("object {name}"),
            Self::Nullable(inner) => format!("nullable<{}>", inner.display_name()),
        }
    }
}

pub(crate) fn access_result_type(member_or_item: &RuleType) -> RuleType {
    RuleType::nullable(member_or_item.clone())
}

/// Source-level definition of one named boolean or value rule. Metadata is
/// adapted into this type by the deploy-time validator; this crate never
/// depends on the metadata crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDefinition {
    pub name: String,
    pub bindings: BTreeMap<String, RuleType>,
    pub result: RuleType,
    pub expression: String,
}

/// The two and only two supported DMN-inspired decision-table policies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HitPolicy {
    First,
    Unique,
    /// This variant lets a deploy-time metadata adapter retain an invalid
    /// source spelling long enough for [`compile_catalog`](crate::compile_catalog)
    /// to return a stable validation error instead of silently falling back.
    Unsupported(String),
}

impl HitPolicy {
    pub fn from_metadata(value: &str) -> Self {
        match value {
            "first" => Self::First,
            "unique" => Self::Unique,
            other => Self::Unsupported(other.to_owned()),
        }
    }
}

/// Ordered source definition for a decision table. This is declarative
/// metadata, not a database relation or a runtime endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTableDefinition {
    pub name: String,
    /// Immutable deployed definition revision used for redacted audit traces.
    pub revision: String,
    pub inputs: BTreeMap<String, RuleType>,
    pub output: BTreeMap<String, RuleType>,
    pub hit_policy: HitPolicy,
    pub rows: Vec<DecisionRow>,
    #[serde(default)]
    pub test_cases: Vec<DecisionTableTestCase>,
}

/// A source row retains its stable identity and optional operator-facing
/// description. Only its declared condition and output shapes are executable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRow {
    pub id: String,
    pub description: Option<String>,
    pub when: BTreeMap<String, String>,
    pub output: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTableTestCase {
    pub name: String,
    pub input: Value,
    pub expect: DecisionTestExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTestExpectation {
    pub output: Value,
    pub matched_row_id: String,
}

/// A parsed and type-checked rule. The checked expression is intentionally
/// private: consumers may evaluate or later lower it, but cannot bypass type
/// validation by constructing an AST at request time.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub name: String,
    pub bindings: BTreeMap<String, RuleType>,
    pub result: RuleType,
    pub(crate) expression: CheckedExpr,
}

#[derive(Debug, Clone)]
pub(crate) struct CheckedExpr {
    pub(crate) expression: Expr,
    pub(crate) type_: CheckedType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckedType {
    Concrete(RuleType),
    Null,
}

/// A redacted audit record for one decision evaluation. It contains definition
/// identifiers and boolean outcomes only; source bindings and output payloads
/// are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionTrace {
    pub table_name: String,
    pub table_revision: String,
    pub matched_row_id: Option<String>,
    pub rejection: Option<DecisionRejection>,
    pub condition_results: Vec<DecisionConditionTrace>,
    pub input_digest: String,
}

/// The non-sensitive decision outcome recorded when no row can be selected.
/// It is intentionally a closed enum rather than an error string so traces
/// cannot accidentally contain raw input or provider details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRejection {
    NoMatch,
    MultipleMatches,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionConditionTrace {
    pub row_id: String,
    pub conditions: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionResult {
    pub output: Value,
    pub matched_row_id: String,
    pub trace: DecisionTrace,
}

#[derive(Debug, Clone)]
pub struct CompiledDecisionTable {
    pub name: String,
    pub revision: String,
    pub(crate) inputs: BTreeMap<String, RuleType>,
    pub(crate) hit_policy: HitPolicy,
    pub rows: Vec<CompiledDecisionRow>,
}

#[derive(Debug, Clone)]
pub struct CompiledDecisionRow {
    pub id: String,
    pub description: Option<String>,
    pub(crate) conditions: BTreeMap<String, CheckedExpr>,
    pub(crate) output: Value,
}

/// The complete compiled snapshot. It stores no raw input data and exposes no
/// ambient runtime capability.
#[derive(Debug, Clone, Default)]
pub struct RuleCatalog {
    pub(crate) rules: BTreeMap<String, CompiledRule>,
    pub(crate) decision_tables: BTreeMap<String, CompiledDecisionTable>,
}

/// Typed, deploy-time or closed-context errors. Variants intentionally carry
/// names, types, row ids, and locations but never raw bindings, expressions,
/// secrets, provider credentials, or serialized JSON values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuleError {
    #[error("{message}")]
    Diagnostic {
        diagnostic: Box<RuleDiagnostic>,
        message: String,
        #[source]
        source: Box<RuleError>,
    },
    #[error(transparent)]
    Parse(#[from] crate::ParseError),
    #[error("duplicate {kind} name `{name}`")]
    DuplicateName { kind: &'static str, name: String },
    #[error("rule `{rule}` uses undeclared binding `{name}`")]
    UndeclaredName { rule: String, name: String },
    #[error("rule `{rule}` references undeclared enum type `{enum_name}`")]
    UnknownEnumType { rule: String, enum_name: String },
    #[error("rule `{rule}` references unknown enum symbol `{enum_name}::{symbol}`")]
    UnknownEnumSymbol {
        rule: String,
        enum_name: String,
        symbol: String,
    },
    #[error("rule `{rule}` cannot access undeclared field `{field}`")]
    UnknownField { rule: String, field: String },
    #[error("rule `{rule}` has incompatible types: expected {expected}, got {actual}")]
    TypeMismatch {
        rule: String,
        expected: String,
        actual: String,
    },
    #[error("rule `{rule}` applies an operation to a nullable value")]
    NullableOperation { rule: String },
    #[error("rule `{rule}` uses an invalid literal for {expected}")]
    InvalidLiteral { rule: String, expected: String },
    #[error("rule `{rule}` produces {actual}, not declared result {expected}")]
    InvalidRuleResult {
        rule: String,
        expected: String,
        actual: String,
    },
    #[error("rule `{rule}` has an incompatible conditional branch")]
    IncompatibleBranches { rule: String },
    #[error("binding `{name}` is required")]
    MissingBinding { name: String },
    #[error("binding `{name}` is not declared")]
    UnexpectedBinding { name: String },
    #[error("binding `{name}` is not valid for its declared type {expected}")]
    InvalidBinding { name: String, expected: String },
    #[error("evaluation of rule `{rule}` attempted division by zero")]
    DivisionByZero { rule: String },
    #[error("decision table `{table}` has unsupported hit policy `{policy}`")]
    UnsupportedHitPolicy { table: String, policy: String },
    #[error("decision table `{table}` has no rows")]
    EmptyDecisionTable { table: String },
    #[error("decision table `{table}` has an invalid condition or output column `{column}`")]
    InvalidDecisionColumn { table: String, column: String },
    #[error("decision table `{table}` is missing its final all-true default row")]
    MissingDefaultRow { table: String },
    #[error("decision table `{table}` has an invalid row `{row_id}`")]
    InvalidDecisionRow { table: String, row_id: String },
    #[error("decision table output field `{field}` is forbidden")]
    ForbiddenDecisionOutput { field: String },
    #[error("decision table `{table}` had no matching row")]
    DecisionNoMatch {
        table: String,
        trace: Box<DecisionTrace>,
    },
    #[error("decision table `{table}` had multiple matching rows")]
    DecisionMultipleMatches {
        table: String,
        row_ids: Vec<String>,
        trace: Box<DecisionTrace>,
    },
    #[error("decision table `{table}` test case `{case_name}` did not match")]
    DecisionTestCaseMismatch { table: String, case_name: String },
    #[error("decision table `{table}` was not found")]
    UnknownDecisionTable { table: String },
    #[error("rule `{rule}` was not found")]
    UnknownRule { rule: String },
    #[error("rule `{rule}` reached an invalid validated expression state")]
    InternalInvariant { rule: String },
}

impl RuleError {
    /// Return the deploy-time semantic location when the compiler has one.
    /// Parsing and runtime evaluation errors intentionally have no diagnostic.
    pub fn diagnostic(&self) -> Option<&RuleDiagnostic> {
        match self {
            Self::Diagnostic { diagnostic, .. } => Some(diagnostic),
            _ => None,
        }
    }

    pub(crate) fn with_diagnostic(self, context: &ExpressionContext, span: Span) -> Self {
        if self.diagnostic().is_some() {
            return self;
        }

        let message = self.to_string();
        Self::Diagnostic {
            diagnostic: Box::new(RuleDiagnostic {
                context: context.clone(),
                span,
                message: message.clone(),
            }),
            message,
            source: Box::new(self),
        }
    }
}
