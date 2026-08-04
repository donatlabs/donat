//! A deliberately small, deterministic CEL-inspired expression language.
//!
//! Parsing is intentionally separate from rule type checking and evaluation.
//! This crate accepts only the deploy-time profile documented in
//! `specs/004-decision-rules.md`; it never evaluates source text at request
//! time and has no ambient access to runtime state.

mod eval;
mod lexer;
mod parser;
mod postgres;
mod types;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use eval::{
    RuleBindings, canonical_bytes, compile_catalog, compile_catalog_with_declared_types,
    compile_catalog_with_declared_types_and_contexts,
    compile_catalog_with_declared_types_and_contexts_and_declaration_order, evaluate_bool,
    evaluate_value,
};
pub use parser::parse_expression;
pub use postgres::{
    PostgresDecisionHitPolicy, PostgresDecisionOutput, PostgresDecisionProgram,
    PostgresDecisionRow, SqlBinding, SqlBindings, SqlExpression, lower_postgres,
    lower_postgres_decision, lower_postgres_expression, lower_postgres_value,
};
pub use types::{
    CanonicalRoot, CanonicalValue, CompiledDecisionRow, CompiledDecisionTable, CompiledRule,
    DecisionConditionTrace, DecisionOutputField, DecisionRejection, DecisionResult, DecisionRow,
    DecisionTableDefinition, DecisionTableTestCase, DecisionTestExpectation, DecisionTrace,
    DefinitionRevision, EvaluatedRuleValue, ExpressionContext, ExpressionOwner, HitPolicy,
    LoweredRuleValue, RuleArtifact, RuleCatalog, RuleDefinition, RuleDiagnostic, RuleError,
    RuleType,
};

/// Profile format version carried in every canonical rule artifact.
pub const PROFILE_VERSION: u16 = 1;
/// Fixed prefix for all canonical profile records.
pub const MAGIC: [u8; 22] = *b"DONAT-RULES-CANONICAL\0";

/// Maximum UTF-8 source size for a single declarative rule expression.
pub const MAX_EXPRESSION_BYTES: usize = 4096;
/// Maximum number of AST nodes on one root-to-leaf path.
pub const MAX_AST_DEPTH: usize = 64;
/// Maximum number of direct elements in a list literal.
pub const MAX_LIST_ITEMS: usize = 256;

/// A half-open source span expressed as UTF-8 byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A parsed expression together with the location of its complete source
/// construct. The span is used by later validation stages to produce stable
/// deploy-time diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

impl Expr {
    pub(crate) fn new(span: Span, kind: ExprKind) -> Self {
        Self { span, kind }
    }
}

/// The closed expression grammar accepted by the Donat CEL profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExprKind {
    Literal(Literal),
    Name(String),
    /// A nominal enum value. The declaration name is retained rather than
    /// reducing the token to a string, so overlapping symbols never compare.
    EnumSymbol {
        enum_name: String,
        symbol: String,
    },
    List(Vec<Expr>),
    Member {
        target: Box<Expr>,
        field: String,
    },
    Index {
        target: Box<Expr>,
        index: usize,
    },
    Call {
        function: Function,
        arguments: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Conditional {
        condition: Box<Expr>,
        when_true: Box<Expr>,
        when_false: Box<Expr>,
    },
}

/// Source literals preserve their decimal spelling until the type checker
/// validates them against a declared rule type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(String),
    Decimal(String),
    String(String),
}

/// The only standard functions available to declarative expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Function {
    Size,
    IsNull,
    StartsWith,
    EndsWith,
}

impl Function {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "size" => Some(Self::Size),
            "is_null" => Some(Self::IsNull),
            "startsWith" => Some(Self::StartsWith),
            "endsWith" => Some(Self::EndsWith),
            _ => None,
        }
    }
}

/// Unary operators supported by the restricted grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Not,
    Negate,
}

/// Binary operators supported by the restricted grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Or,
    And,
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// A stable deploy-time parse diagnostic. Consumers may render the rule name,
/// byte offset, and user-facing expectation without exposing lexer or parser
/// implementation details through GraphQL error payloads.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("rule `{rule_name}` at byte {offset}: expected {expectation}")]
pub struct ParseError {
    pub rule_name: String,
    pub offset: usize,
    pub expectation: String,
}

impl ParseError {
    pub(crate) fn new(
        rule_name: impl Into<String>,
        offset: usize,
        expectation: impl Into<String>,
    ) -> Self {
        Self {
            rule_name: rule_name.into(),
            offset,
            expectation: expectation.into(),
        }
    }
}
