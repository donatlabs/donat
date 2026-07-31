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

use donat_catalog::{Catalog, ColumnInfo};
use donat_ir::RowValidator;
use donat_metadata::{PermissionValidator, Source, TableEntry};
use donat_rules::{
    RuleDefinition, RuleType, SqlBinding, SqlBindings, SqlExpression, compile_catalog,
    lower_postgres,
};

use crate::PlanError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ValidatorOp {
    Insert,
    Update,
}

impl ValidatorOp {
    fn label(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
        }
    }

    /// The CTE holding the rows this operation wrote. Rule lowering happens
    /// here, before SQLgen renders anything, so the alias is a shared contract
    /// rather than a local choice on either side.
    fn row_alias(self) -> &'static str {
        match self {
            Self::Insert => donat_sqlgen::INSERT_ROW_ALIAS,
            Self::Update => donat_sqlgen::UPDATE_ROW_ALIAS,
        }
    }
}

type ValidatorKey = (String, String, ValidatorOp);

/// Compiled validators for one source, keyed by table, role and operation.
///
/// A key that failed to compile retains its diagnostic instead of disappearing:
/// a missing entry means "this role declared no validators", and silently
/// treating a broken declaration as an absent one would drop a check the
/// author wrote.
#[derive(Debug, Default)]
pub(crate) struct ValidatorIndex {
    compiled: HashMap<ValidatorKey, Result<Vec<RowValidator>, String>>,
    errors: Vec<String>,
}

impl ValidatorIndex {
    /// Deploy-time diagnostics. A non-empty list must refuse publication.
    pub(crate) fn errors(&self) -> &[String] {
        &self.errors
    }

    pub(crate) fn get(
        &self,
        table: &str,
        role: &str,
        op: ValidatorOp,
        error_path: &str,
    ) -> Result<Vec<RowValidator>, PlanError> {
        match self.compiled.get(&(table.to_owned(), role.to_owned(), op)) {
            None => Ok(Vec::new()),
            Some(Ok(validators)) => Ok(validators
                .iter()
                .map(|validator| RowValidator {
                    sql: validator.sql.clone(),
                    message: validator.message.clone(),
                    error_path: error_path.to_owned(),
                })
                .collect()),
            Some(Err(message)) => Err(PlanError::validation(error_path, message.clone())),
        }
    }

    /// Compile every `validate` list declared by one source.
    pub(crate) fn build(source: &Source, catalog: &Catalog) -> Self {
        let mut index = Self::default();
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
fn compile_validators(
    table: &str,
    role: &str,
    op: ValidatorOp,
    validators: &[PermissionValidator],
    columns: &[ColumnInfo],
) -> Result<Vec<RowValidator>, String> {
    let where_ = format!("{} {} permission on {table}", role, op.label());
    let mut environment = BTreeMap::new();
    for column in columns {
        if let Some(type_) = rule_type(column) {
            environment.insert(column.name.clone(), type_);
        }
    }

    let mut compiled = Vec::with_capacity(validators.len());
    for (index, validator) in validators.iter().enumerate() {
        let at = format!("{where_}, validate[{index}]");
        if validator.message.trim().is_empty() {
            return Err(format!("{at}: a validator message cannot be empty"));
        }
        match (&validator.not_null, &validator.expression) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "{at}: a validator declares either `expression` or `not_null`, not both"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "{at}: a validator must declare `expression` or `not_null`"
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
                compiled.push(RowValidator {
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
                let sql = match presence_guard {
                    Some(guard) => format!("({guard}) OR ({sql})"),
                    None => sql,
                };
                compiled.push(RowValidator {
                    sql,
                    message: validator.message.clone(),
                    error_path: String::new(),
                });
            }
        }
    }
    Ok(compiled)
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
                .starts_with(r#"("ins"."description" IS NULL) OR ("#),
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
