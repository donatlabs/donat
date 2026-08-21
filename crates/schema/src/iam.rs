//! Compiling in-tenant grants into the predicates a governed role is served
//! through.
//!
//! The shape is one `EXISTS` against the grant relation, comparing the
//! caller's subject, the caller's tenant, and the set of action strings that
//! satisfy this operation on this table. The set is expanded here rather than
//! matched with a pattern at query time: a grant row is compared for equality
//! against a short list, which an index answers, and no pattern a tenant wrote
//! is ever executed as one.
//!
//! Where it lands differs by operation, and the difference is deliberate:
//!
//! | Operation | Where | What a caller without the action sees |
//! |---|---|---|
//! | select | the row predicate | no rows |
//! | insert | the check | a refusal |
//! | update | the check | a refusal |
//! | delete | the row predicate | nothing deleted |
//!
//! Insert and update can refuse because both carry a check the database
//! evaluates over the rows they wrote. A delete carries none, so its gate has
//! to be the predicate — it removes nothing rather than saying no. That
//! asymmetry is in the IR, not in this decision, and it is written down here
//! rather than left for somebody to discover.

use donat_ir::{BoolExp, CompareOp, Scalar, Table};
use donat_metadata::IamOperation;

use crate::PlanError;
use crate::Session;
use crate::plan::{Planner, TableCtx};

impl<'a> Planner<'a> {
    /// The grant predicate for one operation on one table, or `None` when this
    /// deployment has no grants, this role is not governed by them, or the
    /// table is not a tenant's to hold actions on.
    pub(crate) fn iam_predicate(
        &self,
        ctx: &TableCtx,
        session: &Session,
        operation: IamOperation,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(iam) = self.iam else {
            return Ok(None);
        };
        if !iam.governs(&session.role) {
            return Ok(None);
        }
        // Platform reference data belongs to no tenant, so no tenant's role
        // could hold an action over it. Governing it would deny every read of
        // the plan table to exactly the roles that need it.
        if let Some(tenancy) = self.tenancy
            && tenancy.is_shared(&ctx.entry.table)
        {
            return Ok(None);
        }
        // The grant relation itself is read to answer this question. Governing
        // it would need a grant to read grants.
        if same_table(&iam.grants.table, &ctx.entry.table) {
            return Ok(None);
        }

        let Some(info) = self
            .catalog
            .table(iam.grants.table.schema(), iam.grants.table.name())
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!("the grant relation `{}` does not exist", iam.grants.table),
            ));
        };
        let actions = iam.accepted_actions(&ctx.entry.table, operation);
        self.grant_exists(iam, info, session, actions, path)
            .map(Some)
    }

    /// `EXISTS (SELECT 1 FROM <grants> WHERE subject = .. AND tenant = ..
    /// AND action IN (..))`.
    fn grant_exists(
        &self,
        iam: &donat_metadata::IamMetadata,
        info: &donat_catalog_types::TableInfo,
        session: &Session,
        actions: Vec<String>,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let column_type = |name: &str| -> Result<String, PlanError> {
            info.columns
                .iter()
                .find(|column| column.name == name)
                .map(|column| column.sql_type().to_string())
                .ok_or_else(|| {
                    PlanError::new(
                        path,
                        "unexpected",
                        format!(
                            "the grant relation `{}` has no column `{name}`",
                            iam.grants.table
                        ),
                    )
                })
        };
        let subject_key = iam.grants.subject.variable.to_ascii_lowercase();
        let Some(subject) = session.var(&subject_key).filter(|value| !value.is_empty()) else {
            return Err(PlanError::new(
                path,
                "access-denied",
                format!(
                    "this role is served through in-tenant grants and the request carries no \
                     {subject_key}"
                ),
            ));
        };
        let tenant = self.tenant_value(session, path)?;
        let conjuncts = vec![
            BoolExp::Compare {
                column: iam.grants.subject.column.clone(),
                pg_type: column_type(&iam.grants.subject.column)?,
                op: CompareOp::Eq(Scalar::Json(serde_json::Value::String(subject.to_string()))),
            },
            BoolExp::Compare {
                column: iam.grants.tenant.column.clone(),
                pg_type: column_type(&iam.grants.tenant.column)?,
                op: CompareOp::Eq(Scalar::Json(serde_json::Value::String(tenant))),
            },
            BoolExp::Compare {
                column: iam.grants.action.column.clone(),
                pg_type: column_type(&iam.grants.action.column)?,
                op: CompareOp::In(
                    actions
                        .into_iter()
                        .map(|action| Scalar::Json(serde_json::Value::String(action)))
                        .collect(),
                ),
            },
        ];
        Ok(BoolExp::Exists {
            table: Table {
                schema: iam.grants.table.schema().to_string(),
                name: iam.grants.table.name().to_string(),
            },
            predicate: Box::new(BoolExp::And(conjuncts)),
        })
    }

    /// The grant one command invocation needs, or `None` when the caller's
    /// role is not served through grants.
    ///
    /// A command is gated once, as a whole, rather than per step. Its own
    /// `permissions:` list is still the outer gate — this is the narrower one
    /// a tenant writes for itself, which is how "may read orders but may not
    /// cancel one" is expressed without a second compiled role.
    pub(crate) fn command_authorization(
        &self,
        command: &str,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(iam) = self.iam else {
            return Ok(None);
        };
        if !iam.governs(&session.role) {
            return Ok(None);
        }
        let Some(info) = self
            .catalog
            .table(iam.grants.table.schema(), iam.grants.table.name())
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!("the grant relation `{}` does not exist", iam.grants.table),
            ));
        };
        self.grant_exists(
            iam,
            info,
            session,
            iam.accepted_command_actions(command),
            path,
        )
        .map(Some)
    }

    /// The bound on what a tenant may write into its own grant rows.
    ///
    /// This is the sharpest edge in the whole layer: a role able to grant
    /// actions can grant itself anything, including actions that belong to the
    /// platform rather than to any tenant. The reservation is enforced on the
    /// table the tenant writes, as an ordinary check the database evaluates —
    /// not as a rule the command that writes it is trusted to remember.
    ///
    /// A reservation ending in `:*` bars the whole service prefix. The pattern
    /// is a platform declaration rendered by the engine; no value a tenant
    /// supplied is ever executed as one.
    pub(crate) fn reserved_action_bound(
        &self,
        ctx: &TableCtx,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(iam) = self.iam else {
            return Ok(None);
        };
        let Some(target) = &iam.grants.written_via else {
            return Ok(None);
        };
        if !same_table(&target.table, &ctx.entry.table) || iam.reserved_actions.is_empty() {
            return Ok(None);
        }
        let Some(column) = ctx
            .info
            .columns
            .iter()
            .find(|column| column.name == target.action)
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "`{}` has no column `{}` to hold an action",
                    target.table, target.action
                ),
            ));
        };
        let pg_type = column.sql_type().to_string();
        let bounds = iam
            .reserved_actions
            .iter()
            .map(|reserved| match reserved.strip_suffix(":*") {
                Some(service) => BoolExp::Compare {
                    column: target.action.clone(),
                    pg_type: pg_type.clone(),
                    op: CompareOp::Nlike(Scalar::Json(serde_json::Value::String(format!(
                        "{service}:%"
                    )))),
                },
                None => BoolExp::Compare {
                    column: target.action.clone(),
                    pg_type: pg_type.clone(),
                    op: CompareOp::Neq(Scalar::Json(serde_json::Value::String(reserved.clone()))),
                },
            })
            .collect::<Vec<_>>();
        Ok(Some(BoolExp::And(bounds)))
    }

    /// AND a grant predicate onto whatever the caller already had.
    pub(crate) fn with_iam(
        &self,
        existing: Option<BoolExp>,
        ctx: &TableCtx,
        session: &Session,
        operation: IamOperation,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let mut conjuncts = Vec::new();
        if let Some(existing) = existing {
            conjuncts.push(existing);
        }
        if let Some(gate) = self.iam_predicate(ctx, session, operation, path)? {
            conjuncts.push(gate);
        }
        // Only writes can escalate, so only writes carry the reservation.
        if matches!(operation, IamOperation::Insert | IamOperation::Update)
            && let Some(reserved) = self.reserved_action_bound(ctx, path)?
        {
            conjuncts.push(reserved);
        }
        Ok(match conjuncts.len() {
            0 => None,
            1 => conjuncts.pop(),
            _ => Some(BoolExp::And(conjuncts)),
        })
    }
}

fn same_table(
    left: &donat_metadata::QualifiedTable,
    right: &donat_metadata::QualifiedTable,
) -> bool {
    left.schema() == right.schema() && left.name() == right.name()
}
