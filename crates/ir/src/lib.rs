//! Intermediate representation (milestone M3).
//!
//! The hard boundary of the engine: a validated GraphQL operation with
//! permissions already woven in (row filters merged into predicates, column
//! sets restricted, session variables substituted by the planner), expressed
//! without any reference to SQL. Everything above the IR (parser, planner,
//! permissions) is testable without a database; everything below it (sqlgen,
//! executor) is the only code that knows Postgres exists.

mod value_contract;

use std::collections::BTreeMap;

use serde::Serialize;

pub use donat_value_contract::{
    BoundedInlineBytes, CanonicalDecimal, CanonicalNumber, TypeRef, TypedValue,
    VALUE_TYPE_LANGUAGE_VERSION, ValueContractCatalog, ValueContractError, ValueContractField,
    ValueObjectContract, ValueScalar, ValueType, canonical_size,
};
pub use value_contract::{ProcessStartPolicy, compile_value_contract_catalog};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Table {
    pub schema: String,
    pub name: String,
}

/// One root field of a query operation, in selection-set order.
#[derive(Debug, Clone, Serialize)]
pub enum RootField {
    Select {
        alias: String,
        query: SelectQuery,
    },
    /// Relay `<table>_connection` root.
    Connection {
        alias: String,
        conn: Connection,
    },
    /// `__typename` on the root (e.g. `query_root`).
    Typename {
        alias: String,
        value: String,
    },
}

/// A Relay connection over a table: rows of `query` wrapped in
/// edges/pageInfo, with pk-based cursors and global ids.
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// Row source; `fields` are the node's fields.
    pub query: SelectQuery,
    /// Join to the enclosing row for nested relationship connections.
    pub join: Vec<(String, String)>,
    /// Primary key columns: (name, pg_type) — cursor + default order.
    pub pk: Vec<(String, String)>,
    pub schema: String,
    pub table: String,
    /// Connection-level selection, in order.
    pub fields: Vec<ConnectionField>,
    /// Cursor pagination, when first/after/last/before were given.
    pub page: Option<RelayPage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelayPage {
    /// Page size; rows are fetched size+1 to compute has(Next|Previous)Page.
    pub size: u64,
    /// true = last/before (reverse iteration).
    pub backward: bool,
    /// An after/before cursor was given, so the opposite side has pages.
    pub has_other_side: bool,
}

#[derive(Debug, Clone, Serialize)]
pub enum ConnectionField {
    /// (alias, selected pageInfo field names as (alias, name)).
    PageInfo {
        alias: String,
        fields: Vec<(String, String)>,
    },
    Edges {
        alias: String,
        fields: Vec<EdgeField>,
    },
    Typename {
        alias: String,
        value: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum EdgeField {
    Cursor {
        alias: String,
    },
    /// Node renders the connection query's `fields`.
    Node {
        alias: String,
    },
    Typename {
        alias: String,
        value: String,
    },
}

/// What a select reads FROM.
#[derive(Debug, Clone, Serialize)]
pub enum FromSource {
    Table(Table),
    /// Set-returning function with literal arguments (tracked function
    /// root fields): `FROM "schema"."fn"(arg, ...)`.
    Function {
        schema: String,
        name: String,
        /// Rendered in order; `None` name means positional.
        args: Vec<FunctionArgValue>,
    },
    /// Function applied to the enclosing row (table-valued computed
    /// field): `FROM "schema"."fn"("outer".*)`.
    RowFunction {
        schema: String,
        name: String,
        /// Argument list in declared order.
        args: Vec<RowFunctionArg>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionArgValue {
    /// Named-notation argument name, when known.
    pub name: Option<String>,
    pub value: Scalar,
    pub pg_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub enum RowFunctionArg {
    /// The enclosing table's row.
    Row,
    /// The session variables as a json object literal.
    SessionJson(String),
    /// A user-provided extra argument (computed fields with arguments).
    Value { value: Scalar, pg_type: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectQuery {
    pub from: FromSource,
    pub fields: Vec<OutputField>,
    pub predicate: Option<BoolExp>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    /// Permission limit for the `nodes` of an aggregate select: it caps
    /// the rows clients see, but not the aggregate computations.
    pub nodes_limit: Option<u64>,
    pub offset: Option<u64>,
    pub distinct_on: Vec<String>,
    /// `by_pk` roots and object relationships return a single nullable
    /// object instead of a list.
    pub single: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputField {
    /// Response key (GraphQL alias or field name).
    pub alias: String,
    pub value: FieldValue,
}

#[derive(Debug, Clone, Serialize)]
pub enum FieldValue {
    Column {
        column: String,
        /// Postgres type, used by sqlgen for output casts (e.g. timestamps).
        pg_type: String,
    },
    /// Inherited-role cell-level permission: the column is NULL on rows
    /// where none of the granting parent roles' filters pass.
    ColumnGuarded {
        column: String,
        pg_type: String,
        guard: BoolExp,
    },
    /// `__typename`; the planner resolves the concrete type name.
    Typename { value: String },
    /// Object relationship: at most one row from the remote table.
    Object {
        query: SelectQuery,
        /// Join condition: (local column, remote column) pairs.
        join: Vec<(String, String)>,
    },
    /// Array relationship: list of rows from the remote table.
    Array {
        query: SelectQuery,
        join: Vec<(String, String)>,
        /// Aggregate selection (`<rel>_aggregate`) renders differently.
        aggregate: bool,
    },
    /// Aggregate sub-selection on the current table (for `<t>_aggregate`).
    Aggregate { fields: Vec<AggregateField> },
    /// The `nodes` field inside an aggregate selection.
    Nodes { fields: Vec<OutputField> },
    /// Relay global object id: base64 of [1, schema, table, pk...].
    RelayGlobalId {
        schema: String,
        table: String,
        pk: Vec<(String, String)>,
    },
    /// A nested `<rel>_connection`.
    NestedConnection { conn: Box<Connection> },
    /// Placeholder for a remote-schema join, filled in post-processing.
    RemoteJoin { spec: RemoteJoinSpec },
    /// Scalar computed field: `"schema"."fn"("outer".*[, session])`.
    ComputedScalar {
        schema: String,
        name: String,
        args: Vec<RowFunctionArg>,
        /// Inherited-role cell guard.
        guard: Option<Box<BoolExp>>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateField {
    pub alias: String,
    pub op: AggregateOp,
}

#[derive(Debug, Clone, Serialize)]
pub enum AggregateOp {
    /// GraphQL meta-field on `<table>_aggregate_fields`.
    Typename { value: String },
    Count {
        distinct: bool,
        columns: Vec<String>,
    },
    /// sum/avg/min/max over a set of columns.
    ColumnOp {
        op: String,
        columns: Vec<AggregateColumn>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateColumn {
    pub alias: String,
    pub column: String,
    pub pg_type: String,
    /// Inherited-role cell guard: aggregate only cells the role can see.
    pub guard: Option<BoolExp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderBy {
    pub target: OrderByTarget,
    pub direction: OrderDirection,
    pub nulls: NullsOrder,
}

#[derive(Debug, Clone, Serialize)]
pub enum OrderByTarget {
    Column(String),
    /// Order by a column of an object-related table: (join, remote column).
    Relationship {
        table: Table,
        join: Vec<(String, String)>,
        column: String,
        /// The remote table's row filter for the requesting role.
        predicate: Option<Box<BoolExp>>,
    },
    /// Order by an aggregate over an array relationship:
    /// `{ posts_aggregate: { count: desc } }`.
    RelationshipAggregate {
        table: Table,
        join: Vec<(String, String)>,
        /// SQL aggregate function (count, max, sum, ...).
        function: String,
        /// None for count(*).
        column: Option<String>,
        predicate: Option<Box<BoolExp>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum OrderDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NullsOrder {
    First,
    Last,
}

/// A boolean predicate over rows of one table.
#[derive(Debug, Clone, Serialize)]
pub enum BoolExp {
    And(Vec<BoolExp>),
    Or(Vec<BoolExp>),
    Not(Box<BoolExp>),
    Compare {
        column: String,
        pg_type: String,
        op: CompareOp,
    },
    /// Predicate over a related table (`{ author: { name: { _eq: .. } } }`).
    Relationship {
        table: Table,
        join: Vec<(String, String)>,
        predicate: Box<BoolExp>,
    },
    /// Comparison on a scalar computed field: `fn(row) <op> value`.
    ComputedCompare {
        schema: String,
        name: String,
        args: Vec<RowFunctionArg>,
        pg_type: String,
        op: CompareOp,
    },
    /// Predicate over the rows of a table-valued computed field:
    /// `EXISTS (SELECT 1 FROM fn(row) WHERE pred)`.
    RowFunctionExists {
        schema: String,
        name: String,
        args: Vec<RowFunctionArg>,
        predicate: Box<BoolExp>,
    },
    /// `_exists`: an uncorrelated EXISTS over another table.
    Exists {
        table: Table,
        predicate: Box<BoolExp>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum CompareOp {
    Eq(Scalar),
    Neq(Scalar),
    Gt(Scalar),
    Lt(Scalar),
    Gte(Scalar),
    Lte(Scalar),
    In(Vec<Scalar>),
    Nin(Vec<Scalar>),
    Like(Scalar),
    Nlike(Scalar),
    Ilike(Scalar),
    Nilike(Scalar),
    Similar(Scalar),
    Nsimilar(Scalar),
    Regex(Scalar),
    Iregex(Scalar),
    Nregex(Scalar),
    Niregex(Scalar),
    IsNull(bool),
    /// Column-to-column comparison (`_ceq`, `_cgt`, ...): `col <op> other`.
    /// `root` selects the bool_exp's root table (`["$", col]`) instead of
    /// the current one.
    CompareColumn {
        sql_op: String,
        column: String,
        root: bool,
    },
    /// Column compared to a column of an object-related row:
    /// `col <op> (SELECT remote.col FROM ... LIMIT 1)`.
    CompareColumnRel {
        sql_op: String,
        table: Table,
        join: Vec<(String, String)>,
        column: String,
    },
    /// jsonb `?`: has top-level key.
    HasKey(Scalar),
    /// jsonb `?|` / `?&`: has any/all of the keys.
    HasKeysAny(Vec<String>),
    HasKeysAll(Vec<String>),
    /// jsonb `@>` / `<@`.
    Contains(Scalar),
    ContainedIn(Scalar),
    /// PostGIS `ST_<fn>(col, geom)` returning bool.
    StOp {
        function: String,
        value: Scalar,
    },
    /// PostGIS `ST_DWithin(col, geom, distance)` (or the 3D variant).
    StDWithin {
        distance: Scalar,
        from: Scalar,
        three_d: bool,
    },
}

// ---------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------

/// One root field of a mutation operation. Mutations run sequentially in
/// a single transaction, one SQL statement each.
#[derive(Debug, Clone, Serialize)]
pub enum MutationRoot {
    /// A tracked VOLATILE function exposed as a mutation: executes the
    /// function and returns its rows like a select.
    FunctionCall {
        alias: String,
        query: SelectQuery,
    },
    Insert {
        alias: String,
        insert: InsertMutation,
    },
    Update {
        alias: String,
        update: UpdateMutation,
    },
    Delete {
        alias: String,
        delete: DeleteMutation,
    },
    /// A validated declarative command. It is intentionally SQL-free: Task 5
    /// lowers this immutable command declaration to one Postgres statement.
    Command {
        alias: String,
        command: CommandMutation,
    },
    Typename {
        alias: String,
        value: String,
    },
}

/// The fully resolved command payload crossing the planner/renderer boundary.
///
/// SQLgen receives no raw command metadata, catalog, Rule catalog, role, or
/// request session. Every SQL-relevant identifier, concrete type, role
/// predicate/check, Rule expression, and idempotency input is resolved by the
/// source-local planner before this value is constructed.
#[derive(Debug, Clone, Serialize)]
pub struct CommandMutation {
    pub identity: CommandIdentity,
    pub name: String,
    pub steps: Vec<CommandExecutionStep>,
    pub guards: Vec<CommandRule>,
    pub result: Vec<CommandResultField>,
    pub idempotency: Option<CommandIdempotency>,
    pub effects: Vec<ResolvedCommandEffect>,
    pub selection: Vec<CommandResultSelection>,
}

/// Stable execution identity for command journals and durable consumers.
///
/// A metadata name alone is source-local, while one command can authorize
/// multiple classic roles. Keeping all three components distinct prevents a
/// replay or claim elected for one source/role from crossing into another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandIdentity {
    pub source: String,
    pub name: String,
    pub role: String,
}

/// One deploy-time tracked column resolved for a command operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandColumn {
    pub name: String,
    /// The type this column is declared and cast as. For a domain-typed column
    /// this is the schema-qualified domain, so generated SQL keeps the exact
    /// database type.
    pub pg_type: String,
    /// The base type behind `pg_type`, used for every decision about what the
    /// value *means* — which aggregate applies, how it converts to JSON. A
    /// domain is transparent to those decisions; only the cast keeps its name.
    pub logical_type: String,
    pub nullable: bool,
}

impl CommandColumn {
    /// A column whose declared type is already its base type.
    pub fn new(name: String, pg_type: String, nullable: bool) -> Self {
        Self {
            name,
            logical_type: pg_type.clone(),
            pg_type,
            nullable,
        }
    }
}

/// A named command CTE operation with all table and explicit-role facts
/// resolved by the planner.
#[derive(Debug, Clone, Serialize)]
pub enum CommandExecutionStep {
    /// One request argument list decoded to a typed bounded relational CTE.
    /// It is internal to the execution plan and cannot be referenced by
    /// command metadata or exposed as a result step.
    ArgumentRows {
        name: String,
        cte: String,
        items: Scalar,
        columns: Vec<CommandColumn>,
        minimum_items: u32,
        maximum_items: u32,
        error_path: String,
    },
    SelectOne {
        name: String,
        cte: String,
        table: Table,
        by: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        require_found: bool,
        filter: Option<BoolExp>,
        error_path: String,
    },
    SelectMany {
        name: String,
        cte: String,
        table: Table,
        equality: Vec<CommandAssignment>,
        order_by: Vec<CommandColumn>,
        returning: Vec<CommandColumn>,
        require_non_empty: bool,
        filter: Option<BoolExp>,
        error_path: String,
    },
    Aggregate {
        name: String,
        cte: String,
        input_cte: String,
        values: Vec<CommandAggregateIr>,
        error_path: String,
    },
    Project {
        name: String,
        cte: String,
        values: Vec<CommandNamedValue>,
        error_path: String,
    },
    ProjectMany {
        name: String,
        cte: String,
        input_cte: String,
        maximum_rows: u32,
        values: Vec<CommandNamedValue>,
        error_path: String,
    },
    FixedRows {
        name: String,
        cte: String,
        maximum_rows: u32,
        columns: Vec<CommandColumn>,
        rows: Vec<Vec<CommandExecutionValue>>,
        error_path: String,
    },
    Decision {
        name: String,
        cte: String,
        decision: CommandDecision,
        input: Vec<CommandNamedValue>,
        returning: Vec<CommandColumn>,
        error_path: String,
    },
    DecisionMany {
        name: String,
        cte: String,
        input_cte: String,
        decision: CommandDecision,
        input: Vec<CommandNamedValue>,
        returning: Vec<CommandColumn>,
        order_by: Vec<CommandColumn>,
        error_path: String,
    },
    Insert {
        name: String,
        cte: String,
        table: Table,
        object: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        check: Option<BoolExp>,
        error_path: String,
    },
    InsertMany {
        name: String,
        cte: String,
        table: Table,
        items: Scalar,
        item_fields: Vec<CommandColumn>,
        object: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        allow_empty: bool,
        check: Option<BoolExp>,
        error_path: String,
    },
    InsertRows {
        name: String,
        cte: String,
        table: Table,
        input_cte: String,
        where_nonzero: Option<String>,
        item_fields: Vec<CommandColumn>,
        object: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        allow_empty: bool,
        check: Option<BoolExp>,
        error_path: String,
    },
    Update {
        name: String,
        cte: String,
        table: Table,
        predicate: Vec<CommandAssignment>,
        set: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        require_affected: bool,
        filter: Option<BoolExp>,
        check: Option<BoolExp>,
        error_path: String,
    },
    UpdateWhen {
        name: String,
        cte: String,
        condition: CommandCondition,
        table: Table,
        predicate: Vec<CommandAssignment>,
        set: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        require_affected: bool,
        filter: Option<BoolExp>,
        check: Option<BoolExp>,
        error_path: String,
    },
    UpdateMany {
        name: String,
        cte: String,
        table: Table,
        input_cte: String,
        primary_key: Vec<CommandAssignment>,
        guards: Vec<CommandAssignment>,
        assignments: Vec<CommandAssignment>,
        check: Option<CommandRule>,
        returning: Vec<CommandColumn>,
        require_each: bool,
        filter: Option<BoolExp>,
        permission_check: Option<BoolExp>,
        error_path: String,
    },
    Delete {
        name: String,
        cte: String,
        table: Table,
        predicate: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        require_affected: bool,
        filter: Option<BoolExp>,
        error_path: String,
    },
    Assert {
        name: String,
        rule: CommandRule,
    },
    AssertWhen {
        name: String,
        condition: CommandCondition,
        rule: CommandRule,
    },
    InsertWhen {
        name: String,
        cte: String,
        condition: CommandCondition,
        table: Table,
        object: Vec<CommandAssignment>,
        returning: Vec<CommandColumn>,
        check: Option<BoolExp>,
        error_path: String,
    },
    AllocateMany {
        name: String,
        cte: String,
        input_cte: String,
        request_id: CommandExecutionValue,
        group_key: Vec<CommandColumn>,
        requested: CommandColumn,
        available: CommandColumn,
        allocated: CommandColumn,
        backordered: CommandColumn,
        groups: Vec<CommandColumn>,
        lines: Vec<CommandColumn>,
        backorders: Vec<CommandColumn>,
        group_order_by: Vec<CommandColumn>,
        line_order_by: Vec<CommandColumn>,
        maximum_rows: u32,
        error_path: String,
    },
}

/// One named field produced by a pure projection or supplied to a resolved
/// decision. Names and values are both deployment-validated; SQLgen receives
/// no raw command value grammar.
#[derive(Debug, Clone, Serialize)]
pub struct CommandNamedValue {
    pub name: String,
    pub column: CommandColumn,
    pub value: CommandExecutionValue,
}

/// Immutable identity and output contract of one compiled decision table.
/// The definition revision prevents request planning from silently crossing a
/// deployment while Task 4 adds the renderer-owned canonical row program.
#[derive(Debug, Clone, Serialize)]
pub struct CommandDecision {
    pub name: String,
    pub revision: String,
    pub hit_policy: CommandDecisionHitPolicy,
    pub rows: Vec<CommandDecisionRow>,
}

/// Closed decision hit policy copied from the Rules-owned lowered program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CommandDecisionHitPolicy {
    First,
    Unique,
}

/// One already-lowered decision row. SQLgen receives no raw decision
/// metadata, checked expression, or Rule catalog.
#[derive(Debug, Clone, Serialize)]
pub struct CommandDecisionRow {
    pub id: String,
    pub condition_sql: String,
    pub output: Vec<CommandDecisionOutput>,
}

/// One typed business output literal lowered by `donat-rules`.
#[derive(Debug, Clone, Serialize)]
pub struct CommandDecisionOutput {
    pub name: String,
    pub sql: String,
    pub column: CommandColumn,
}

/// A condition is retained as typed data so SQLgen can materialize a gate.
/// The request planner must never branch in Rust on this value.
#[derive(Debug, Clone, Serialize)]
pub enum CommandCondition {
    ArgumentEquals {
        argument: Scalar,
        expected: Scalar,
        pg_type: String,
    },
}

/// One named, catalog-typed aggregation over a prior command row set.
#[derive(Debug, Clone, Serialize)]
pub enum CommandAggregateIr {
    Count {
        output: CommandColumn,
    },
    Sum {
        output: CommandColumn,
        input: CommandColumn,
    },
    Min {
        output: CommandColumn,
        input: CommandColumn,
    },
    Max {
        output: CommandColumn,
        input: CommandColumn,
    },
    CountDistinct {
        output: CommandColumn,
        input: CommandColumn,
    },
}

impl CommandAggregateIr {
    pub fn output(&self) -> &CommandColumn {
        match self {
            Self::Count { output }
            | Self::Sum { output, .. }
            | Self::Min { output, .. }
            | Self::Max { output, .. }
            | Self::CountDistinct { output, .. } => output,
        }
    }
}

/// One concrete target-column assignment in a command operation.
#[derive(Debug, Clone, Serialize)]
pub struct CommandAssignment {
    pub column: CommandColumn,
    pub value: CommandExecutionValue,
}

/// A typed, closed source for a command value. The renderer never interprets
/// an argument name, metadata literal, Rule name, or session-variable name.
#[derive(Debug, Clone, Serialize)]
pub enum CommandExecutionValue {
    Scalar {
        value: Scalar,
        pg_type: String,
    },
    StepColumn {
        cte: String,
        column: CommandColumn,
    },
    StepRows {
        cte: String,
        columns: Vec<CommandColumn>,
    },
    StepFieldRows {
        cte: String,
        field: String,
        columns: Vec<CommandColumn>,
        where_nonzero: Option<String>,
    },
    Item {
        field: String,
        pg_type: String,
    },
    CurrentColumn {
        column: CommandColumn,
    },
    Rule {
        sql: String,
        pg_type: String,
    },
    DatabaseTime {
        function: CommandDatabaseTime,
        pg_type: String,
    },
}

/// Closed database-clock functions available to commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CommandDatabaseTime {
    Now,
}

/// A Rule expression lowered from the immutable Rules catalog with closed SQL
/// bindings. `sql` comes only from `donat-rules`; SQLgen does not parse rule
/// source or resolve a rule name.
#[derive(Debug, Clone, Serialize)]
pub struct CommandRule {
    pub sql: String,
    pub pg_type: String,
    pub error_path: String,
    pub message: String,
}

/// One immutable declared result field, after resolving its producer.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResultField {
    pub name: String,
    pub value: CommandResultValue,
}

/// Closed source for a declared result field.
#[derive(Debug, Clone, Serialize)]
pub enum CommandResultValue {
    StepRow {
        cte: String,
        many: bool,
        /// Fixed declared `returning` columns used for the canonical result.
        columns: Vec<CommandColumn>,
    },
    StepColumn {
        cte: String,
        column: CommandColumn,
    },
    Scalar {
        value: Scalar,
        pg_type: String,
    },
    Rule {
        sql: String,
        pg_type: String,
    },
    ProjectedRows {
        cte: String,
        /// Whether the source CTE carries `_cmd_ordinal`. The public value is
        /// always a bounded list; a scalar source contributes zero or one row.
        many: bool,
        columns: Vec<CommandResultProjection>,
        maximum_items: u32,
    },
    Array {
        value: Scalar,
        maximum_items: u32,
    },
}

/// One declared result-object field projected from a concrete step column.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResultProjection {
    pub name: String,
    pub source: CommandColumn,
}

/// Inputs that are already resolved before SQLgen computes an idempotency
/// fingerprint or journal lookup. Scope order is metadata order.
#[derive(Debug, Clone, Serialize)]
pub struct CommandIdempotency {
    pub key: Scalar,
    pub scope: Vec<CommandExecutionValue>,
    pub input: Scalar,
    pub retention_seconds: Option<u64>,
    pub error_path: String,
}

/// One Process hand-off after the source-local Process revision and every
/// request value have been resolved. SQLgen receives no raw effect metadata.
#[derive(Debug, Clone, Serialize)]
pub enum ResolvedCommandEffect {
    StartProcess(ResolvedStartProcessEffect),
    SignalProcess(ResolvedSignalProcessEffect),
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedStartProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub start_policy: ProcessStartPolicy,
    pub input: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    /// Present only when the executing command role is one of the Process's
    /// declared caller roles. The map contains the compiler-published closed
    /// subset of session variables, never the ambient request map.
    pub caller_role: Option<String>,
    pub caller_session_variables: BTreeMap<String, CommandExecutionValue>,
    pub command_invocation_id: CommandInvocationIdSource,
    pub effect_position: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedSignalProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub signal_name: String,
    pub correlation: BTreeMap<String, CommandExecutionValue>,
    pub payload: BTreeMap<String, CommandExecutionValue>,
    pub semantic_idempotency_key: CommandExecutionValue,
    pub command_invocation_id: CommandInvocationIdSource,
    pub effect_position: u32,
}

/// Closed reference to the generation elected by this command statement.
/// Metadata can never choose or supply an arbitrary UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CommandInvocationIdSource {
    CurrentExecution,
}

/// Client projection of a declared command result. Commands do not expose
/// arbitrary table fields: every selection is rooted in the command's fixed
/// result mapping and, for row values, the step's fixed `returning` list.
#[derive(Debug, Clone, Serialize)]
pub enum CommandResultSelection {
    Scalar {
        alias: String,
        field: String,
    },
    Object {
        alias: String,
        field: String,
        selections: Vec<CommandResultSelection>,
    },
    List {
        alias: String,
        field: String,
        selections: Vec<CommandResultSelection>,
    },
    Typename {
        alias: String,
        value: String,
    },
}

/// One compiled entry of a write permission's `validate` list.
///
/// `sql` is a Postgres boolean over the written-row CTE, produced either by
/// `donat-rules` lowering or by a null test the planner rendered itself.
/// Neither the rule source nor the column name reaches SQLgen as text: this
/// mirrors [`CommandRule`], which made the same guarantee for commands.
///
/// A validator passes only when the boolean is TRUE. An unknown value is a
/// violation, which is what makes a null-handling mistake fail closed.
#[derive(Debug, Clone, Serialize)]
pub struct RowValidator {
    pub sql: String,
    pub message: String,
    pub error_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InsertMutation {
    pub table: Table,
    /// Insertion columns: (name, pg_type).
    pub columns: Vec<(String, String)>,
    /// Row values aligned with `columns`; None renders DEFAULT.
    pub rows: Vec<Vec<Option<Scalar>>>,
    /// Object relationships inserted after the parent row, using values
    /// returned from the parent INSERT for the relationship column mapping.
    pub nested_object_inserts: Vec<NestedObjectInsert>,
    pub on_conflict: Option<OnConflict>,
    /// The role's insert check expression, evaluated over inserted rows.
    pub check: Option<BoolExp>,
    /// Error path reported on check violation.
    pub check_path: String,
    /// Ordered per-role value validators over the inserted rows. The first
    /// violated entry is the one reported.
    pub validators: Vec<RowValidator>,
    pub output: MutationOutput,
}

#[derive(Debug, Clone, Serialize)]
pub struct NestedObjectInsert {
    pub relationship_name: String,
    pub table: Table,
    /// (parent column, child column) pairs.
    pub column_mapping: Vec<(String, String)>,
    /// Child insertion columns supplied by the user, excluding mapped columns.
    pub columns: Vec<(String, String)>,
    /// Child row values aligned with `columns`.
    pub row: Vec<Option<Scalar>>,
    pub check: Option<BoolExp>,
    pub check_path: String,
    pub validators: Vec<RowValidator>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnConflict {
    pub constraint: String,
    pub update_columns: Vec<String>,
    /// Condition over the existing row for DO UPDATE.
    pub predicate: Option<BoolExp>,
    /// Update-permission presets applied on conflict.
    pub set_ops: Vec<SetOp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateMutation {
    pub table: Table,
    pub sets: Vec<SetOp>,
    /// User where AND permission filter.
    pub predicate: Option<BoolExp>,
    /// The role's post-update check expression.
    pub check: Option<BoolExp>,
    /// Error path reported on check violation.
    pub check_path: String,
    /// Ordered per-role value validators over the updated rows.
    pub validators: Vec<RowValidator>,
    pub output: MutationOutput,
}

#[derive(Debug, Clone, Serialize)]
pub enum SetOp {
    Set {
        column: String,
        pg_type: String,
        value: Scalar,
    },
    Inc {
        column: String,
        pg_type: String,
        value: Scalar,
    },
    JsonbAppend {
        column: String,
        value: Scalar,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteMutation {
    pub table: Table,
    pub predicate: Option<BoolExp>,
    pub output: MutationOutput,
}

/// What a mutation returns.
#[derive(Debug, Clone, Serialize)]
pub enum MutationOutput {
    /// `{ affected_rows, returning [...] }` in selection order.
    Response(Vec<MutationResponseField>),
    /// `<t>_by_pk` / `insert_<t>_one`: the (nullable) row itself.
    SingleRow(Vec<OutputField>),
}

#[derive(Debug, Clone, Serialize)]
pub enum MutationResponseField {
    AffectedRows {
        alias: String,
    },
    Returning {
        alias: String,
        fields: Vec<OutputField>,
    },
    Typename {
        alias: String,
        value: String,
    },
}

/// A remote-schema join resolved after local execution: for each row,
/// run `query` against the remote schema with variables taken from the
/// row's hidden columns, then graft the response under the field alias.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteJoinSpec {
    pub schema: String,
    /// Operation with variable definitions, e.g.
    /// `query($v0: Int!) { message(id: $v0) { name } }`.
    pub query: String,
    /// (variable name, hidden row key) pairs.
    pub variables: Vec<(String, String)>,
    /// The remote root field whose value is grafted.
    pub root_field: String,
}

/// A literal that reaches SQL. Session variables are substituted by the
/// planner before the IR is final, so sqlgen only ever sees literals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Scalar {
    Json(serde_json::Value),
}

impl Scalar {
    pub fn as_json(&self) -> &serde_json::Value {
        match self {
            Scalar::Json(v) => v,
        }
    }
}
