//! Deploy-time compilation of write-permission `validate` lists.
//!
//! A validator is a per-role value check over the row a mutation wrote. It is
//! compiled once, when the planner index for a source is built, and never at
//! request time: the rule profile is a deploy-time language, and a mutation
//! must not pay for parsing CEL on every call.
//!
//! Two failure modes are deliberately distinct. Metadata that does not compile
//! is a deployment error — it is reported by `donat validate` and refuses
//! publication, so a serving engine never holds an unusable validator. If such
//! a key is nevertheless reached, the planner returns the retained diagnostic
//! as an ordinary plan error. Neither path can panic, and neither produces a
//! 500: the value that reaches the planner is a `Result`, not an `unwrap`.

use std::collections::{BTreeMap, HashMap};

use donat_catalog_types::{Catalog, ColumnInfo};
use donat_ir::RowValidator;
use donat_metadata::{PermissionValidator, PhoneRegion, Source, TableEntry};
use donat_rules::{
    RuleDefinition, RuleType, SqlBinding, SqlBindings, SqlExpression, compile_catalog,
    lower_postgres,
};

use crate::PlanError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ValidatorOp {
    Insert,
    Update,
    /// The update permission's list, lowered against the INSERT CTE.
    ///
    /// An upsert writes its DO UPDATE rows through the insert statement, so
    /// the update contract has to be read off the same rows the insert
    /// returns. Same list, same messages, different alias.
    UpsertUpdate,
}

impl ValidatorOp {
    fn label(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update | Self::UpsertUpdate => "update",
        }
    }

    /// The CTE holding the rows this operation wrote. Rule lowering happens
    /// here, before SQLgen renders anything, so the alias is a shared contract
    /// rather than a local choice on either side.
    fn row_alias(self) -> &'static str {
        match self {
            Self::Insert | Self::UpsertUpdate => donat_sqlgen::INSERT_ROW_ALIAS,
            Self::Update => donat_sqlgen::UPDATE_ROW_ALIAS,
        }
    }
}

type ValidatorKey = (String, String, ValidatorOp);

/// One compiled `phone` entry.
///
/// Unlike the other spellings this never becomes SQL. The planner applies it
/// to the value it is about to put in the statement: a rejection is a plan
/// error carrying the entry's message, and an acceptance replaces the value
/// with its E.164 form. The region is a [`PhoneRegion`], which can only be
/// built by parsing a declared region code — the type is the proof that
/// nothing on the request can reach it.
#[derive(Debug, Clone)]
pub(crate) struct PhoneCheck {
    pub(crate) column: String,
    pub(crate) region: PhoneRegion,
    pub(crate) message: String,
    pub(crate) error_path: String,
}

impl PhoneCheck {
    /// The E.164 form of one submitted value, or `None` when there is nothing
    /// to normalize because the caller sent a null.
    ///
    /// A rejection carries the entry's own message under `validation-failed`,
    /// which is the shape every validator reports with — the caller cannot
    /// tell from the response that this one was evaluated in Rust.
    fn normalize(&self, value: &serde_json::Value) -> Result<Option<serde_json::Value>, PlanError> {
        if value.is_null() {
            return Ok(None);
        }
        let refused = || PlanError::validation(&self.error_path, self.message.clone());
        let submitted = value.as_str().ok_or_else(refused)?;
        let normalized =
            donat_metadata::normalize_phone(submitted, &self.region).map_err(|_| refused())?;
        Ok(Some(serde_json::Value::String(normalized)))
    }
}

/// One permission's compiled value contract: the gates the statement carries,
/// and the checks the planner runs before there is a statement.
///
/// They travel together because they are one ordered `validate` list, and
/// because every consumer that asks "does this permission constrain values"
/// must see both halves — a caller that only looked at `rows` would treat a
/// phone-only list as no list at all.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompiledValidators {
    pub(crate) rows: Vec<RowValidator>,
    pub(crate) phone: Vec<PhoneCheck>,
}

impl CompiledValidators {
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.phone.is_empty()
    }

    /// Check and rewrite the values an insert is about to carry.
    ///
    /// This runs on the planner's own data, before any SQL exists, and it is
    /// the whole runtime cost of a `phone` validator: one parse per submitted
    /// value. The statement it produces is the statement it would have
    /// produced anyway, with a normalized literal in place of the submitted
    /// one.
    ///
    /// A column the caller did not write, and a null, are not violations —
    /// there is no value to reject. Presence is declared with a `not_null`
    /// entry, exactly as it is for every other spelling.
    pub(crate) fn normalize_rows(
        &self,
        columns: &[(String, String)],
        rows: &mut [Vec<Option<donat_ir::Scalar>>],
    ) -> Result<(), PlanError> {
        for check in &self.phone {
            let Some(index) = columns.iter().position(|(name, _)| name == &check.column) else {
                continue;
            };
            for row in rows.iter_mut() {
                let Some(slot) = row.get_mut(index) else {
                    continue;
                };
                let Some(value) = slot.as_ref() else { continue };
                if let Some(normalized) = check.normalize(value.as_json())? {
                    *slot = Some(donat_ir::Scalar::Json(normalized));
                }
            }
        }
        Ok(())
    }

    /// The same, for the `_set` shape an update writes.
    ///
    /// `_inc` and `_append` are not reached: neither is expressible over the
    /// text column a phone number lives in.
    pub(crate) fn normalize_sets(&self, sets: &mut [donat_ir::SetOp]) -> Result<(), PlanError> {
        for check in &self.phone {
            for set in sets.iter_mut() {
                let donat_ir::SetOp::Set { column, value, .. } = set else {
                    continue;
                };
                if column != &check.column {
                    continue;
                }
                if let Some(normalized) = check.normalize(value.as_json())? {
                    *value = donat_ir::Scalar::Json(normalized);
                }
            }
        }
        Ok(())
    }

    fn with_error_path(&self, error_path: &str) -> Self {
        Self {
            rows: self
                .rows
                .iter()
                .map(|validator| RowValidator {
                    sql: validator.sql.clone(),
                    message: validator.message.clone(),
                    error_path: error_path.to_owned(),
                })
                .collect(),
            phone: self
                .phone
                .iter()
                .map(|check| PhoneCheck {
                    error_path: error_path.to_owned(),
                    ..check.clone()
                })
                .collect(),
        }
    }
}

/// Compiled validators for one source, keyed by table, role and operation.
///
/// A key that failed to compile retains its diagnostic instead of disappearing:
/// a missing entry means "this role declared no validators", and silently
/// treating a broken declaration as an absent one would drop a check the
/// author wrote.
#[derive(Debug, Default)]
pub(crate) struct ValidatorIndex {
    compiled: HashMap<ValidatorKey, Result<CompiledValidators, String>>,
    errors: Vec<String>,
}

impl ValidatorIndex {
    /// Deploy-time diagnostics. A non-empty list must refuse publication.
    pub(crate) fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The validators of one already-resolved permission.
    ///
    /// `role` is the role that *declared* the permission, not the role on the
    /// request. Those differ under role inheritance: a child role with no
    /// entry of its own is granted the parent's permission wholesale, and
    /// keying on the request role would hand it an empty validator list —
    /// silently dropping the parent's checks for the child. The caller
    /// therefore passes the declaring role, which it already knows because it
    /// resolved the permission.
    pub(crate) fn get(
        &self,
        table: &str,
        role: &str,
        op: ValidatorOp,
        error_path: &str,
    ) -> Result<CompiledValidators, PlanError> {
        match self.compiled.get(&(table.to_owned(), role.to_owned(), op)) {
            None => Ok(CompiledValidators::default()),
            Some(Ok(validators)) => Ok(validators.with_error_path(error_path)),
            Some(Err(message)) => Err(PlanError::validation(error_path, message.clone())),
        }
    }

    /// Compile every `validate` list declared by one source.
    pub(crate) fn build(source: &Source, catalog: &Catalog) -> Self {
        let mut index = Self::default();
        // Only the Postgres mutation renderer emits the gate. SQLite and
        // MySQL have their own mutation executors that never read the
        // compiled list, and their introspection reports pg-shaped type names
        // — so a validate list there would compile cleanly and then be
        // dropped on the floor. Refusing is the difference between "not
        // supported yet" and "silently not enforced".
        let postgres = matches!(source.kind, donat_metadata::SourceKind::Postgres);
        for entry in &source.tables {
            let key = format!("{}.{}", entry.table.schema(), entry.table.name());
            // Command permissions reuse the same shapes (ADR-019), so they
            // parse a `validate` list — but command steps write through their
            // own per-step CTEs, which this index does not name, and the
            // command planner does not consult it. Accepting the key would
            // mean silently ignoring a check the author wrote, which is the
            // one failure mode a permission plane must not have.
            for (role, validators) in entry
                .command_insert_permissions
                .iter()
                .map(|permission| (&permission.role, &permission.permission.validate))
                .chain(
                    entry
                        .command_update_permissions
                        .iter()
                        .map(|permission| (&permission.role, &permission.permission.validate)),
                )
            {
                if !validators.is_empty() {
                    index.errors.push(format!(
                        "{role} command permission on {key}: `validate` is not supported on a command permission; put the check in the command's own `assert` step"
                    ));
                }
            }
            if !postgres {
                let declared = entry
                    .insert_permissions
                    .iter()
                    .map(|permission| (&permission.role, &permission.permission.validate))
                    .chain(
                        entry
                            .update_permissions
                            .iter()
                            .map(|permission| (&permission.role, &permission.permission.validate)),
                    )
                    .find(|(_, validators)| !validators.is_empty());
                if let Some((role, _)) = declared {
                    index.errors.push(format!(
                        "{role} permission on {key}: `validate` is supported only on a Postgres source; this source is {:?}",
                        source.kind
                    ));
                }
                continue;
            }
            let Some(columns) = catalog_columns(catalog, entry) else {
                // An untracked or not-yet-introspected table cannot type an
                // expression. Ordinary planning already refuses it, so there
                // is nothing to add here.
                continue;
            };
            for permission in &entry.insert_permissions {
                index.insert_key(
                    &key,
                    &permission.role,
                    ValidatorOp::Insert,
                    &permission.permission.validate,
                    columns,
                );
            }
            for permission in &entry.update_permissions {
                index.insert_key(
                    &key,
                    &permission.role,
                    ValidatorOp::UpsertUpdate,
                    &permission.permission.validate,
                    columns,
                );
                index.insert_key(
                    &key,
                    &permission.role,
                    ValidatorOp::Update,
                    &permission.permission.validate,
                    columns,
                );
            }
        }
        index
    }

    fn insert_key(
        &mut self,
        table: &str,
        role: &str,
        op: ValidatorOp,
        validators: &[PermissionValidator],
        columns: &[ColumnInfo],
    ) {
        if validators.is_empty() {
            return;
        }
        let compiled = compile_validators(table, role, op, validators, columns);
        if let Err(message) = &compiled {
            self.errors.push(message.clone());
        }
        self.compiled
            .insert((table.to_owned(), role.to_owned(), op), compiled);
    }
}

/// Command steps that would fall back to an ordinary permission carrying a
/// `validate` list.
///
/// `resolve_command_role_perm` prefers a command permission and falls back to
/// the ordinary one when the role declares none. The ordinary permissions are
/// exactly the ones that carry validators, and the command planner never
/// consults this index — so the fallback would apply the permission's columns,
/// filter, check and presets while quietly dropping its value contract.
///
/// The scan is deliberately conservative: it ignores role inheritance when
/// deciding whether a command permission exists, so it can refuse a
/// deployment that would in fact have been safe, but it cannot miss one that
/// would not.
pub(crate) fn command_fallback_errors(
    source: &Source,
    commands: &[donat_metadata::Command],
) -> Vec<String> {
    use donat_metadata::CommandStepOperation as Step;

    let mut errors = Vec::new();
    for command in commands.iter().filter(|c| c.source == source.name) {
        for step in &command.steps {
            let (table, op) = match &step.operation {
                Step::Insert { insert } => (&insert.table, ValidatorOp::Insert),
                Step::InsertMany { insert_many } => (&insert_many.table, ValidatorOp::Insert),
                Step::InsertWhen { insert_when } => (&insert_when.table, ValidatorOp::Insert),
                Step::Update { update } => (&update.table, ValidatorOp::Update),
                Step::UpdateMany { update_many } => (&update_many.table, ValidatorOp::Update),
                Step::UpdateWhen { update_when } => (&update_when.table, ValidatorOp::Update),
                _ => continue,
            };
            let Some(entry) = source.tables.iter().find(|entry| {
                entry.table.schema() == table.schema() && entry.table.name() == table.name()
            }) else {
                continue;
            };
            let (command_list, ordinary): (Vec<&String>, Vec<(&String, &[PermissionValidator])>) =
                match op {
                    ValidatorOp::Insert => (
                        entry
                            .command_insert_permissions
                            .iter()
                            .map(|permission| &permission.role)
                            .collect(),
                        entry
                            .insert_permissions
                            .iter()
                            .map(|permission| {
                                (&permission.role, permission.permission.validate.as_slice())
                            })
                            .collect(),
                    ),
                    ValidatorOp::Update | ValidatorOp::UpsertUpdate => (
                        entry
                            .command_update_permissions
                            .iter()
                            .map(|permission| &permission.role)
                            .collect(),
                        entry
                            .update_permissions
                            .iter()
                            .map(|permission| {
                                (&permission.role, permission.permission.validate.as_slice())
                            })
                            .collect(),
                    ),
                };
            for invoker in &command.permissions {
                if command_list.iter().any(|role| **role == invoker.role) {
                    continue;
                }
                let falls_back_to_validators = ordinary
                    .iter()
                    .any(|(role, validators)| **role == invoker.role && !validators.is_empty());
                if falls_back_to_validators {
                    errors.push(format!(
                        "command '{}' step '{}' writes {}.{} as role {}, which would fall back to an ordinary {} permission carrying a `validate` list; that list cannot be enforced on a command step — state the rule in an `assert` step, or give the role its own command permission",
                        command.name,
                        step.name,
                        table.schema(),
                        table.name(),
                        invoker.role,
                        op.label(),
                    ));
                }
            }
        }
    }
    errors
}

fn catalog_columns<'a>(catalog: &'a Catalog, entry: &TableEntry) -> Option<&'a [ColumnInfo]> {
    catalog
        .tables
        .get(&format!("{}.{}", entry.table.schema(), entry.table.name()))
        .map(|table| table.columns.as_slice())
}

/// Compile one ordered `validate` list.
///
/// The type environment starts as the table's columns, each with the
/// catalogue's own nullability. A `not_null` entry rebinds its column to the
/// non-null type for the entries that follow it — which is the only way a
/// later comparison over a nullable column can type check, because the rule
/// profile refuses operations on nullable operands and has no flow-sensitive
/// refinement to discover the guard on its own.
///
/// A `phone` entry is compiled the same way and lands somewhere else: it
/// resolves its declared region once, here, and the planner evaluates it in
/// Rust over the submitted value. Ordering therefore holds among entries of
/// the same kind, and a phone rejection precedes any expression gate — see the
/// module documentation of `phone` in `donat-metadata`.
fn compile_validators(
    table: &str,
    role: &str,
    op: ValidatorOp,
    validators: &[PermissionValidator],
    columns: &[ColumnInfo],
) -> Result<CompiledValidators, String> {
    let where_ = format!("{} {} permission on {table}", role, op.label());
    let mut environment = BTreeMap::new();
    for column in columns {
        if let Some(type_) = rule_type(column) {
            environment.insert(column.name.clone(), type_);
        }
    }

    let mut compiled = CompiledValidators::default();
    for (index, validator) in validators.iter().enumerate() {
        let at = format!("{where_}, validate[{index}]");
        if validator.message.trim().is_empty() {
            return Err(format!("{at}: a validator message cannot be empty"));
        }
        if let Some(phone) = &validator.phone {
            if validator.expression.is_some() || validator.not_null.is_some() {
                return Err(format!(
                    "{at}: a validator declares `phone` or one of `expression` and `not_null`, not both"
                ));
            }
            if validator.when_present.is_some() {
                return Err(format!(
                    "{at}: `when_present` scopes an `expression`; a `phone` validator already ignores a value that is not there"
                ));
            }
            compiled
                .phone
                .push(compile_phone(&at, phone, &validator.message, columns)?);
            continue;
        }
        match (&validator.not_null, &validator.expression) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "{at}: a validator declares either `expression` or `not_null`, not both"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "{at}: a validator must declare `expression`, `not_null` or `phone`"
                ));
            }
            (Some(_), None) if validator.when_present.is_some() => {
                return Err(format!(
                    "{at}: `when_present` scopes an `expression`; it says nothing about `not_null`"
                ));
            }
            (Some(column_name), None) => {
                let column = columns
                    .iter()
                    .find(|column| &column.name == column_name)
                    .ok_or_else(|| {
                        format!("{at}: `not_null` names unknown column '{column_name}'")
                    })?;
                if !column.nullable {
                    return Err(format!(
                        "{at}: column '{column_name}' is already NOT NULL in the database, so this validator can never fire"
                    ));
                }
                if let Some(type_) = rule_type(column) {
                    environment.insert(column_name.clone(), type_.into_inner());
                }
                compiled.rows.push(RowValidator {
                    sql: format!(
                        "{} IS NOT NULL",
                        donat_sqlgen::rule_qualified_column(op.row_alias(), column_name)
                    ),
                    message: validator.message.clone(),
                    error_path: String::new(),
                });
            }
            (None, Some(expression)) => {
                // `when_present` refines exactly one column for exactly this
                // entry: the rows it excludes are the rows where that column
                // is null, so inside the expression the value is known to be
                // there. The refinement does not leak to later entries, which
                // is what keeps it different from `not_null`.
                let mut environment = environment.clone();
                let mut presence_guard = None;
                if let Some(column_name) = &validator.when_present {
                    let column = columns
                        .iter()
                        .find(|column| &column.name == column_name)
                        .ok_or_else(|| {
                            format!("{at}: `when_present` names unknown column '{column_name}'")
                        })?;
                    if !column.nullable {
                        return Err(format!(
                            "{at}: column '{column_name}' is NOT NULL in the database, so it is always present"
                        ));
                    }
                    if let Some(type_) = rule_type(column) {
                        environment.insert(column_name.clone(), type_.into_inner());
                    }
                    presence_guard = Some(format!(
                        "{} IS NULL",
                        donat_sqlgen::rule_qualified_column(op.row_alias(), column_name)
                    ));
                }
                let name = format!("{table}:{role}:{}:validate[{index}]", op.label());
                let definition = RuleDefinition {
                    name: name.clone(),
                    bindings: environment.clone(),
                    result: RuleType::Bool,
                    expression: expression.clone(),
                };
                let catalog = compile_catalog(std::slice::from_ref(&definition), &[])
                    .map_err(|error| format!("{at}: {error}"))?;
                let rule = catalog
                    .rule(&name)
                    .ok_or_else(|| format!("{at}: the expression did not compile"))?;
                let bindings = SqlBindings::new(environment.iter().map(|(column, type_)| {
                    (
                        column.clone(),
                        SqlBinding::expression(SqlExpression::column(
                            op.row_alias(),
                            column,
                            type_.clone(),
                        )),
                    )
                }));
                let sql =
                    lower_postgres(rule, &bindings).map_err(|error| format!("{at}: {error}"))?;
                // A CASE rather than `OR`: Postgres does not promise to
                // evaluate `OR` left to right, so a lowered expression that
                // can raise on a NULL operand would raise instead of being
                // skipped. The profile cannot produce such an expression
                // today; the CASE costs nothing and does not depend on that
                // staying true.
                let sql = match presence_guard {
                    Some(guard) => format!("CASE WHEN {guard} THEN TRUE ELSE ({sql}) END"),
                    None => sql,
                };
                compiled.rows.push(RowValidator {
                    sql,
                    message: validator.message.clone(),
                    error_path: String::new(),
                });
            }
        }
    }
    Ok(compiled)
}

/// Compile one `phone` entry.
///
/// Everything that can be settled at deploy time is settled here: the column
/// exists and holds text, and the declared region resolves. What is left for
/// request time is one parse of one string.
///
/// The operation is not a parameter, unlike every other spelling: this check
/// reads the value the caller submitted rather than a column of a CTE, and
/// that value is the same whichever statement will carry it.
fn compile_phone(
    at: &str,
    phone: &donat_metadata::PhoneValidator,
    message: &str,
    columns: &[ColumnInfo],
) -> Result<PhoneCheck, String> {
    let column = columns
        .iter()
        .find(|column| column.name == phone.column)
        .ok_or_else(|| format!("{at}: `phone` names unknown column '{}'", phone.column))?;
    // The normalized value is an E.164 string, so the column has to be able
    // to hold one. A numeric column would silently lose the leading `+` and
    // the country code's leading zeros.
    if !matches!(
        column.pg_type.as_str(),
        "text" | "varchar" | "bpchar" | "citext"
    ) {
        return Err(format!(
            "{at}: column '{}' is {}, and a phone number is stored as its E.164 text",
            phone.column, column.pg_type
        ));
    }
    let region = PhoneRegion::parse(&phone.region).map_err(|error| format!("{at}: {error}"))?;
    Ok(PhoneCheck {
        column: phone.column.clone(),
        region,
        message: message.to_owned(),
        // Filled in by `ValidatorIndex::get`, which knows the operation's path.
        error_path: String::new(),
    })
}

/// The rule type of one catalogue column, or `None` when the column has no
/// representation in the rule profile.
///
/// An unrepresentable column is simply absent from the environment, so an
/// expression that mentions it fails with the profile's own "undeclared
/// binding" diagnostic instead of a type invented here.
fn rule_type(column: &ColumnInfo) -> Option<RuleType> {
    let base = match column.pg_type.as_str() {
        "bool" => RuleType::Bool,
        "int2" | "int4" => RuleType::Int,
        "int8" => RuleType::Int64,
        "numeric" | "decimal" => RuleType::Decimal,
        "text" | "varchar" | "bpchar" | "name" | "citext" => RuleType::String,
        "uuid" => RuleType::Uuid,
        "date" => RuleType::Date,
        "timestamptz" | "timestamp with time zone" => RuleType::Timestamp,
        _ => return None,
    };
    Some(if column.nullable {
        RuleType::nullable(base)
    } else {
        base
    })
}

trait IntoInner {
    fn into_inner(self) -> RuleType;
}

impl IntoInner for RuleType {
    /// Strip one nullable wrapper. `not_null` has already proven the value is
    /// present for every row the gate lets through.
    fn into_inner(self) -> RuleType {
        match self {
            RuleType::Nullable(inner) => *inner,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, pg_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            pg_type: pg_type.to_owned(),
            pg_typmod: -1,
            native_type: None,
            nullable,
            has_default: false,
        }
    }

    fn columns() -> Vec<ColumnInfo> {
        vec![
            column("quality_grade", "int2", true),
            column("title", "text", false),
            column("description", "text", true),
        ]
    }

    fn validator(
        expression: Option<&str>,
        not_null: Option<&str>,
        when_present: Option<&str>,
        message: &str,
    ) -> PermissionValidator {
        PermissionValidator {
            expression: expression.map(str::to_owned),
            not_null: not_null.map(str::to_owned),
            when_present: when_present.map(str::to_owned),
            phone: None,
            message: message.to_owned(),
        }
    }

    /// A `phone` entry over the same table, for the tests that mix spellings.
    fn phone_validator(column: &str, region: &str, message: &str) -> PermissionValidator {
        PermissionValidator {
            expression: None,
            not_null: None,
            when_present: None,
            phone: Some(donat_metadata::PhoneValidator {
                column: column.to_owned(),
                region: region.to_owned(),
            }),
            message: message.to_owned(),
        }
    }

    fn compile(validators: &[PermissionValidator]) -> Result<Vec<RowValidator>, String> {
        compile_validators(
            "public.product_variant",
            "staff",
            ValidatorOp::Insert,
            validators,
            &columns(),
        )
        .map(|compiled| compiled.rows)
    }

    /// The case that must never reach a request: a comparison over a nullable
    /// column with nothing said about the null. It is a deployment error, it
    /// names where it came from, and it is a value — not a panic, and not a
    /// 500 waiting to happen at request time.
    #[test]
    fn an_undeclared_null_is_a_deployment_error_not_a_panic() {
        let error = compile(&[validator(
            Some("quality_grade > 3"),
            None,
            None,
            "quality_grade must be greater than 3",
        )])
        .expect_err("a nullable operand cannot be compared without declaring the null");

        assert!(error.contains("public.product_variant"), "{error}");
        assert!(error.contains("staff insert permission"), "{error}");
        assert!(error.contains("validate[0]"), "{error}");
        assert!(error.contains("nullable"), "{error}");
    }

    /// The same failure reaching the planner is an ordinary plan error, so a
    /// request against a key that did not compile is refused rather than
    /// crashing the process.
    #[test]
    fn a_failed_key_is_refused_at_plan_time_without_panicking() {
        let mut index = ValidatorIndex::default();
        index.compiled.insert(
            (
                "public.product_variant".to_owned(),
                "staff".to_owned(),
                ValidatorOp::Insert,
            ),
            Err("deliberately broken".to_owned()),
        );

        let error = index
            .get(
                "public.product_variant",
                "staff",
                ValidatorOp::Insert,
                "$.selectionSet.insert_product_variant.args.objects",
            )
            .expect_err("a key that did not compile must not plan");
        assert_eq!(
            error.path,
            "$.selectionSet.insert_product_variant.args.objects"
        );
        assert!(error.message.contains("deliberately broken"));
    }

    /// A role that declared nothing gets nothing. This is the only reason an
    /// absent key may be treated as "no validators".
    #[test]
    fn an_absent_key_yields_no_validators() {
        let index = ValidatorIndex::default();
        let validators = index
            .get("public.product", "staff", ValidatorOp::Insert, "$")
            .expect("a table with no validate list plans normally");
        assert!(validators.is_empty());
    }

    #[test]
    fn not_null_refines_the_column_for_the_entries_that_follow() {
        let compiled = compile(&[
            validator(
                None,
                Some("quality_grade"),
                None,
                "quality_grade cannot be null",
            ),
            validator(
                Some("quality_grade > 3"),
                None,
                None,
                "quality_grade must be greater than 3",
            ),
        ])
        .expect("a declared null makes the comparison typeable");

        assert_eq!(compiled.len(), 2);
        assert_eq!(compiled[0].sql, r#""ins"."quality_grade" IS NOT NULL"#);
        assert!(
            compiled[1].sql.contains(r#""ins"."quality_grade""#),
            "{}",
            compiled[1].sql
        );
        assert_eq!(compiled[0].message, "quality_grade cannot be null");
    }

    #[test]
    fn when_present_excludes_the_null_rows_from_one_entry_only() {
        let compiled = compile(&[validator(
            Some("size(description) >= 20"),
            None,
            Some("description"),
            "description must be at least 20 characters when present",
        )])
        .expect("a declared optional column is readable inside its own entry");

        assert!(
            compiled[0]
                .sql
                .starts_with(r#"CASE WHEN "ins"."description" IS NULL THEN TRUE ELSE ("#),
            "{}",
            compiled[0].sql
        );

        // The refinement does not leak: a following entry sees the column as
        // nullable again, which is what makes `when_present` narrower than
        // `not_null`.
        let error = compile(&[
            validator(
                Some("size(description) >= 20"),
                None,
                Some("description"),
                "description must be at least 20 characters when present",
            ),
            validator(
                Some("size(description) <= 400"),
                None,
                None,
                "description is limited to 400 characters",
            ),
        ])
        .expect_err("a later entry must declare presence for itself");
        assert!(error.contains("validate[1]"), "{error}");
    }

    #[test]
    fn a_non_null_column_needs_no_declaration() {
        let compiled = compile(&[validator(
            Some("size(title) >= 3"),
            None,
            None,
            "title must be at least 3 characters",
        )])
        .expect("a NOT NULL column is readable as it stands");
        assert!(compiled[0].sql.contains(r#""ins"."title""#));
    }

    #[test]
    fn declaring_presence_of_a_non_null_column_is_rejected() {
        let error = compile(&[validator(None, Some("title"), None, "title cannot be null")])
            .expect_err("a NOT NULL column can never be null, so the entry is dead metadata");
        assert!(error.contains("already NOT NULL"), "{error}");

        let error = compile(&[validator(
            Some("size(title) >= 3"),
            None,
            Some("title"),
            "title must be at least 3 characters",
        )])
        .expect_err("the same holds for the when_present spelling");
        assert!(error.contains("always present"), "{error}");
    }

    #[test]
    fn a_validator_names_exactly_one_spelling_over_a_known_column() {
        let error = compile(&[validator(
            Some("size(title) >= 3"),
            Some("title"),
            None,
            "m",
        )])
        .expect_err("two spellings in one entry are ambiguous");
        assert!(error.contains("not both"), "{error}");

        let error = compile(&[validator(None, None, None, "m")])
            .expect_err("an entry with no predicate checks nothing");
        assert!(error.contains("must declare"), "{error}");

        let error = compile(&[validator(None, Some("absent"), None, "m")])
            .expect_err("a validator cannot name a column the table does not have");
        assert!(error.contains("unknown column"), "{error}");

        let error = compile(&[validator(Some("size(title) >= 3"), None, None, "  ")])
            .expect_err("a validator with no message has no error to report");
        assert!(error.contains("message"), "{error}");
    }

    /// A `phone` entry produces no SQL at all — that is the point of it. What
    /// it produces is a resolved region and a column name for the planner to
    /// apply before it builds the statement.
    #[test]
    fn a_phone_validator_compiles_to_a_check_and_no_gate() {
        let compiled = compile_validators(
            "public.contact",
            "user",
            ValidatorOp::Insert,
            &[phone_validator(
                "phone",
                "DE",
                "phone must be a valid number",
            )],
            &[column("phone", "text", true)],
        )
        .expect("a phone validator over a text column compiles");

        assert!(
            compiled.rows.is_empty(),
            "a phone validator adds nothing to the statement"
        );
        assert_eq!(compiled.phone.len(), 1);
        assert_eq!(compiled.phone[0].column, "phone");
        assert_eq!(compiled.phone[0].region.as_str(), "DE");
        assert_eq!(compiled.phone[0].message, "phone must be a valid number");
    }

    /// Everything a `phone` entry cannot do is a deployment error, named where
    /// it was written. In particular a region that could only be resolved from
    /// a request refuses publication instead of being resolved from one.
    #[test]
    fn a_phone_validator_that_cannot_be_evaluated_refuses_publication() {
        let columns = [column("phone", "text", true), column("age", "int4", true)];
        let compile_one = |validator: PermissionValidator| {
            compile_validators(
                "public.contact",
                "user",
                ValidatorOp::Insert,
                &[validator],
                &columns,
            )
            .expect_err("this declaration cannot be evaluated")
        };

        let error = compile_one(phone_validator("absent", "DE", "m"));
        assert!(error.contains("unknown column 'absent'"), "{error}");
        assert!(error.contains("validate[0]"), "{error}");

        let error = compile_one(phone_validator("age", "DE", "m"));
        assert!(error.contains("E.164 text"), "{error}");

        for deferred in ["X-Donat-Region", "de", "DEU", ""] {
            let error = compile_one(phone_validator("phone", deferred, "m"));
            assert!(error.contains("is not a region code"), "{error}");
        }

        let mut both = phone_validator("phone", "DE", "m");
        both.expression = Some("size(phone) >= 3".to_owned());
        let error = compile_one(both);
        assert!(error.contains("not both"), "{error}");

        let mut scoped = phone_validator("phone", "DE", "m");
        scoped.when_present = Some("phone".to_owned());
        let error = compile_one(scoped);
        assert!(error.contains("already ignores"), "{error}");

        let mut unnamed = phone_validator("phone", "DE", "  ");
        unnamed.message = "  ".to_owned();
        let error = compile_one(unnamed);
        assert!(error.contains("message"), "{error}");
    }

    /// An expression over a column the profile cannot type fails with the
    /// profile's own diagnostic, rather than a type invented here.
    #[test]
    fn an_untypeable_column_is_simply_undeclared() {
        let error = compile_validators(
            "public.thing",
            "staff",
            ValidatorOp::Insert,
            &[validator(Some("payload == \"x\""), None, None, "m")],
            &[column("payload", "jsonb", false)],
        )
        .expect_err("a jsonb column has no rule type");
        assert!(error.contains("undeclared"), "{error}");
    }
}

#[cfg(test)]
mod command_permission_tests {
    use super::*;
    /// A `validate` list on a command permission must refuse the deployment.
    /// Command steps write through their own CTEs, so this index cannot
    /// enforce it — and accepting the key would drop a declared check.
    #[test]
    fn a_validate_list_on_a_command_permission_refuses_publication() {
        let source: Source = serde_yaml::from_str(
            r#"
name: default
kind: postgres
configuration: {}
tables:
  - table: { schema: public, name: audit_entry }
    command_insert_permissions:
      - role: fulfilment
        permission:
          check: {}
          validate:
            - expression: "size(note) >= 3"
              message: note must be at least 3 characters
"#,
        )
        .expect("command permissions accept the same shapes as ordinary ones");

        let index = ValidatorIndex::build(&source, &Catalog::default());
        let error = index
            .errors()
            .first()
            .expect("a command permission cannot carry a validate list");
        assert!(error.contains("public.audit_entry"), "{error}");
        assert!(
            error.contains("not supported on a command permission"),
            "{error}"
        );
    }
}

#[cfg(test)]
mod fail_closed_tests {
    use super::*;

    fn source_yaml(body: &str) -> Source {
        serde_yaml::from_str(body).expect("source metadata parses")
    }

    /// A `validate` list on a non-Postgres source compiles — the rule types
    /// resolve, because SQLite introspection reports pg-shaped type names —
    /// but only the Postgres renderer emits the gate. Compiling and then
    /// dropping it is the failure mode this refusal exists to prevent.
    #[test]
    fn a_non_postgres_source_refuses_a_validate_list() {
        let source = source_yaml(
            r#"
name: analytics
kind: sqlite
configuration: {}
tables:
  - table: { schema: public, name: note }
    insert_permissions:
      - role: author
        permission:
          check: {}
          validate:
            - expression: "size(body) >= 3"
              message: body must be at least 3 characters
"#,
        );
        let index = ValidatorIndex::build(&source, &Catalog::default());
        let error = index
            .errors()
            .first()
            .expect("a validate list on SQLite must refuse publication");
        assert!(error.contains("only on a Postgres source"), "{error}");
        assert!(error.contains("public.note"), "{error}");
    }

    /// `resolve_command_role_perm` falls back to the ordinary permission when
    /// the role declares no command permission — and the ordinary permission
    /// is the one that carries validators. The command planner does not
    /// consult this index, so the fallback would apply the columns, filter,
    /// check and presets while dropping the value contract.
    #[test]
    fn a_command_step_falling_back_to_a_validated_permission_refuses_publication() {
        let source = source_yaml(
            r#"
name: default
kind: postgres
configuration: {}
tables:
  - table: { schema: public, name: cart_line }
    insert_permissions:
      - role: customer
        permission:
          check: {}
          validate:
            - expression: "quantity <= 20"
              message: a cart line is limited to 20 units
"#,
        );
        let commands: Vec<donat_metadata::Command> = serde_yaml::from_str(
            r#"
- name: add_to_cart
  source: default
  permissions:
    - role: customer
  steps:
    - name: line
      insert:
        table: { schema: public, name: cart_line }
        object: {}
"#,
        )
        .expect("command metadata parses");

        let errors = command_fallback_errors(&source, &commands);
        let error = errors
            .first()
            .expect("the fallback would drop the customer's value contract");
        assert!(error.contains("add_to_cart"), "{error}");
        assert!(error.contains("cart_line"), "{error}");
        assert!(error.contains("assert"), "{error}");

        // Giving the role its own command permission removes the fallback,
        // and with it the reason to refuse.
        let mut source = source;
        source.tables[0].command_insert_permissions = source.tables[0].insert_permissions.clone();
        source.tables[0].command_insert_permissions[0]
            .permission
            .validate
            .clear();
        assert!(command_fallback_errors(&source, &commands).is_empty());
    }

    /// An upsert writes its DO UPDATE rows through the INSERT statement, so
    /// the update contract has to be readable off the insert CTE.
    #[test]
    fn the_update_list_is_also_compiled_against_the_insert_alias() {
        let columns = vec![ColumnInfo {
            name: "quantity".to_owned(),
            pg_type: "int4".to_owned(),
            pg_typmod: -1,
            native_type: None,
            nullable: false,
            has_default: false,
        }];
        let validators = [PermissionValidator {
            expression: Some("quantity <= 20".to_owned()),
            not_null: None,
            when_present: None,
            phone: None,
            message: "a cart line is limited to 20 units".to_owned(),
        }];

        let upd = compile_validators(
            "public.cart_line",
            "customer",
            ValidatorOp::Update,
            &validators,
            &columns,
        )
        .expect("the update spelling compiles");
        let upsert = compile_validators(
            "public.cart_line",
            "customer",
            ValidatorOp::UpsertUpdate,
            &validators,
            &columns,
        )
        .expect("the same list compiles against the insert alias");

        assert!(
            upd.rows[0].sql.contains(r#""upd"."quantity""#),
            "{}",
            upd.rows[0].sql
        );
        assert!(
            upsert.rows[0].sql.contains(r#""ins"."quantity""#),
            "{}",
            upsert.rows[0].sql
        );
        assert_eq!(upd.rows[0].message, upsert.rows[0].message);
    }
}
