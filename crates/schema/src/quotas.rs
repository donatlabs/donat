//! Compiling a plan ceiling into the write it gates.
//!
//! The whole design of this is one sentence: the counter moves inside the
//! statement that performs the write. Counting first and writing second is the
//! version everybody writes and the version that does not hold — under READ
//! COMMITTED a statement's snapshot is fixed before it executes, so fifty
//! concurrent writers all read the same pre-lock count and all pass it. The
//! `UPDATE` on the tenant's usage row takes a lock instead, and the writers
//! that queue behind it re-read what the previous one committed.

use donat_ir::{QuotaConsumption, Table};
use donat_metadata::TableScope;

use crate::PlanError;
use crate::Session;
use crate::plan::{Planner, TableCtx};

impl<'a> Planner<'a> {
    /// The entitlement this write consumes (`consumes = true`) or releases,
    /// or `None` when nothing caps this table.
    pub(crate) fn quota_consumption(
        &self,
        ctx: &TableCtx,
        session: &Session,
        consumes: bool,
        path: &str,
    ) -> Result<Option<Box<QuotaConsumption>>, PlanError> {
        let Some(quotas) = self.quotas else {
            return Ok(None);
        };
        let Some(entitlement) = quotas.consumed_by(&ctx.entry.table) else {
            return Ok(None);
        };
        let tenancy = self.tenancy.ok_or_else(|| {
            PlanError::new(
                path,
                "unexpected",
                "quotas are declared without tenancy, which deploy-time validation refuses",
            )
        })?;
        let tenant = self.tenant_value(session, path)?;

        // How a tenant is found in the registry that names its plan. The
        // registry row's own identifier is what the tenancy declaration
        // already calls its key, so this follows from that rather than being
        // stated twice.
        let matched_on = match &quotas.limits.via.matched_on {
            Some(column) => column.clone(),
            None => match tenancy.table_scope(&quotas.limits.via.table) {
                TableScope::Key(key) => key.to_string(),
                _ => {
                    return Err(PlanError::new(
                        path,
                        "unexpected",
                        format!(
                            "`{}` is exempt from tenancy, so a tenant cannot be found in it",
                            quotas.limits.via.table
                        ),
                    ));
                }
            },
        };

        Ok(Some(Box::new(QuotaConsumption {
            counters: table_of(&quotas.counters.table),
            counter_column: entitlement.counter.clone(),
            tenant_column: quotas.counters.tenant.column.clone(),
            tenant,
            consumes,
            limits: table_of(&quotas.limits.table),
            limit_key_column: quotas.limits.key.column.clone(),
            maximum_column: entitlement.maximum.clone(),
            registry: table_of(&quotas.limits.via.table),
            registry_plan_column: quotas.limits.via.column.clone(),
            registry_match_column: matched_on,
            error_path: path.to_string(),
            message: format!(
                "this plan allows no more {} for this tenant",
                entitlement.name
            ),
        })))
    }
}

fn table_of(table: &donat_metadata::QualifiedTable) -> Table {
    Table {
        schema: table.schema().to_string(),
        name: table.name().to_string(),
    }
}
