//! The half of the tenancy declaration that only a database can answer.
//!
//! `crates/metadata` decides everything structural: that the source exists,
//! that a table is keyed or exempt and never both, that an exemption says why
//! it is one. What it cannot decide is whether the tenant column a table is
//! *assumed* to carry actually exists, because a table nobody mentioned in
//! `keys:` or `exempt:` carries the default key by rule rather than by
//! declaration. That rule is what makes tracking a table enough to scope it,
//! and it is exactly why the column has to be proved here: without this check,
//! a table missing its `tenant_id` would compile into a predicate against a
//! column that is not there, and the failure would arrive as a 500 on the
//! first query rather than as a refusal to deploy.
//!
//! Both entry points run this: `donat validate` collects the problems for an
//! operator, and the boot compile refuses to publish a snapshot that fails it.

use std::collections::HashMap;

use donat_catalog_types::Catalog;
use donat_metadata::{Metadata, QualifiedTable, TableScope, TenancyMetadata};

use crate::PlanError;

/// Every tenancy rule that needs an introspected catalog, for one deployment.
///
/// Tables the catalog does not have are skipped: a tracked table that does not
/// exist is already reported by the caller, and repeating it here would bury
/// the tenancy problems in noise.
pub fn validate_tenancy_catalog(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
) -> Vec<PlanError> {
    let Some(tenancy) = &metadata.tenancy else {
        return Vec::new();
    };
    let Some(catalog) = catalogs.get(&tenancy.source) else {
        return Vec::new();
    };
    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == tenancy.source)
    else {
        return Vec::new();
    };

    let mut errors = Vec::new();

    // The registry's own key is the type every tenant column is compared
    // against, so it is resolved first and every other column is measured by
    // it. A `text` tenant key pointing at a `uuid` registry is two columns that
    // never match and a tenant that silently sees nothing.
    let registry_type = column_type(catalog, &tenancy.registry.table, &tenancy.registry.key);
    if registry_type.is_none() && catalog_has(catalog, &tenancy.registry.table) {
        errors.push(PlanError::validation(
            "tenancy.registry.key",
            format!(
                "the registry `{}` has no column `{}`",
                tenancy.registry.table, tenancy.registry.key
            ),
        ));
    }
    if catalog_has(catalog, &tenancy.registry.table)
        && column_type(
            catalog,
            &tenancy.registry.table,
            &tenancy.registry.status.column,
        )
        .is_none()
    {
        errors.push(PlanError::validation(
            "tenancy.registry.status.column",
            format!(
                "the registry `{}` has no column `{}`",
                tenancy.registry.table, tenancy.registry.status.column
            ),
        ));
    }

    for entry in &source.tables {
        if !catalog_has(catalog, &entry.table) {
            continue;
        }
        let TableScope::Key(key) = tenancy.table_scope(&entry.table) else {
            continue;
        };
        match column_type(catalog, &entry.table, key) {
            None => errors.push(missing_key_error(tenancy, &entry.table, key)),
            Some(found) => {
                if let Some(expected) = registry_type.as_deref()
                    && found != expected
                {
                    errors.push(PlanError::validation(
                        &format!("tenancy.tables.{}", entry.table),
                        format!(
                            "tenant key \"{}.{key}\" is {found}, but the registry `{}` identifies \
                             a tenant with {expected}. Two columns of different types never \
                             compare equal, so this tenant would see nothing rather than fail.",
                            entry.table, tenancy.registry.table
                        ),
                    ));
                }
            }
        }
    }

    // The counters table is read as a scalar subquery — `(SELECT used FROM
    // __donat_quota)` — so a tenant with two usage rows turns every gated write
    // into PostgreSQL's own "more than one row returned by a subquery" instead
    // of a Donat error shape. One row per tenant is what the design assumes;
    // this is where the database is asked to agree.
    if let Some(quotas) = &metadata.quotas {
        let counters = &quotas.counters.table;
        let column = &quotas.counters.tenant.column;
        match catalog.table(counters.schema(), counters.name()) {
            None => errors.push(PlanError::validation(
                &format!("quotas.counters.table.{counters}"),
                format!(
                    "counters table \"{counters}\" is absent from source `{}`",
                    tenancy.source
                ),
            )),
            Some(info) => {
                let one_per_tenant = info.primary_key == [column.clone()]
                    || info
                        .unique_keys
                        .iter()
                        .any(|key| key == std::slice::from_ref(column));
                if !one_per_tenant {
                    errors.push(PlanError::validation(
                        &format!("quotas.counters.table.{counters}"),
                        format!(
                            "\"{counters}.{column}\" is not unique, so a tenant may have two \
                             usage rows. The counter is read as a single value, so the second \
                             row turns every write this gates into a database error rather than \
                             a refusal. Add a unique constraint on \"{column}\"."
                        ),
                    ));
                }
            }
        }
    }

    errors
}

/// The refusal an operator is most likely to meet, so it says what to do.
///
/// Three answers are correct and the message names all of them, because the
/// wrong reflex here — dropping the table out of tracking, or exempting it to
/// make the error go away — is the one that produces a leak.
fn missing_key_error(tenancy: &TenancyMetadata, table: &QualifiedTable, key: &str) -> PlanError {
    PlanError::validation(
        &format!("tenancy.tables.{table}"),
        format!(
            "tracked table \"{table}\" has no tenant key column \"{key}\". Add the column, or \
             declare the column it uses instead under `tenancy.keys`, or — only if the rows \
             genuinely belong to no single tenant — declare it under `tenancy.exempt` saying \
             why. Serving it unscoped is not one of the options: every reader of this table \
             would see every tenant's rows.\n\nsource: {}",
            tenancy.source
        ),
    )
}

fn catalog_has(catalog: &Catalog, table: &QualifiedTable) -> bool {
    catalog.table(table.schema(), table.name()).is_some()
}

fn column_type(catalog: &Catalog, table: &QualifiedTable, column: &str) -> Option<String> {
    catalog
        .table(table.schema(), table.name())?
        .columns
        .iter()
        .find(|candidate| candidate.name == column)
        .map(|candidate| candidate.sql_type().to_string())
}

// ---------------------------------------------------------------------------
// The predicate
// ---------------------------------------------------------------------------

use donat_ir::{BoolExp, CompareOp, Scalar, Table};
use donat_metadata::QualifiedTable as MetadataTable;

use crate::Session;
use crate::plan::{Planner, TableCtx};

/// Which authorization a write's check carries.
///
/// The two differ in exactly one place, and the difference is easy to get
/// backwards: a command step skips the *table's* action because the command
/// already proved its own, but it must still carry the reservation, because
/// that one bounds what the row may say rather than who may write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckAuthorization {
    /// Ordinary CRUD: the table's own action gates the write.
    Table(donat_metadata::IamOperation),
    /// One step of a command, already authorized by the command's action.
    CommandStep(donat_metadata::IamOperation),
}

/// Where a tenant value comes from when a predicate needs one.
///
/// Every tenant predicate is built the same way from any arm. The session
/// arm is the ordinary case: the claim a verified token carried. The step
/// arms are a command whose tenant is not the caller's — one that reads it
/// off a row an unscoped lookup found, or one that establishes it — and whose
/// tenant therefore lives in the CTE of the step that resolved it. That step
/// is single-row by construction, so `(SELECT <column> FROM <cte> LIMIT 1)`
/// is a scalar and compares exactly as a literal does
/// (`knowledgebase/declarative-saas/decisions/101-*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantRef {
    /// The caller's tenant claim.
    Session,
    /// The tenant column of a single-row command step that has already run.
    Step { cte: String, column: String },
    /// The tenant column of the step that *created* the tenant in this very
    /// statement. Bounds like [`TenantRef::Step`]; not gated by the registry,
    /// because the registry row lives in a data-modifying CTE the rest of the
    /// statement cannot see — it is the row this command is writing.
    Established { cte: String, column: String },
}

impl TenantRef {
    /// Whether the registry's serving gate is read for this tenant.
    fn gated(&self) -> bool {
        !matches!(self, TenantRef::Established { .. })
    }
}

/// Where a write's check gets its tenant, and whether it needs to state the
/// bound again.
///
/// Two questions that look like one. *Whose tenant is this* decides what the
/// registry's status gate compares against — the session's claim, or the value
/// a command step resolved. *Is the bound already stated* decides whether the
/// check repeats it: an update and a delete carry it in their predicate, so
/// repeating it in the check would be noise, while an insert has no predicate
/// at all and the check is the only place it can go — unless a preset already
/// pins the column, which is what a step-sourced tenant does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckTenant {
    /// The caller's session supplies it, and the check states the bound.
    Session,
    /// The caller's session supplies it, and the predicate already stated it.
    SessionBoundElsewhere,
    /// A step of this command supplies it. The bound is the preset (insert)
    /// or the predicate (update, delete); the gate is read by that value.
    Step { cte: String, column: String },
    /// The tenant is created by this very statement: this write is the
    /// establishing step, or one after it. The bound, where there is one, is
    /// the preset; there is no registry row to gate on that the statement can
    /// see, because the row being written is the registry row.
    Establishing,
}

impl CheckTenant {
    fn repeats_the_bound(&self) -> bool {
        matches!(self, CheckTenant::Session)
    }

    /// What the serving gate compares against, if it applies at all.
    fn gate_tenant(&self) -> Option<TenantRef> {
        match self {
            CheckTenant::Session | CheckTenant::SessionBoundElsewhere => Some(TenantRef::Session),
            CheckTenant::Step { cte, column } => Some(TenantRef::Step {
                cte: cte.clone(),
                column: column.clone(),
            }),
            CheckTenant::Establishing => None,
        }
    }
}

impl<'a> Planner<'a> {
    /// The tenant predicate for one table, or `None` when this source is not
    /// tenanted or the table is platform reference data.
    ///
    /// This is the whole isolation guarantee in one function. It is called
    /// from [`Planner::permission_predicate`] rather than from the twelve
    /// places that ask for a row filter, because a guarantee applied at twelve
    /// call sites is a guarantee that is one new call site away from a leak.
    ///
    /// `tenant` says where the value lives. Every ordinary read passes the
    /// session arm. The command plane is the one caller that passes the step
    /// arm: from the step a command takes its tenant from onward, every read
    /// is bounded by what that step resolved rather than by the session, which
    /// for such a command carries no tenant at all.
    pub(crate) fn tenant_predicate(
        &self,
        ctx: &TableCtx,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(tenancy) = self.tenancy else {
            return Ok(None);
        };
        // A declared cross-tenant read replaces the tenant bound with a
        // subject bound. It never removes one: the row is still restricted,
        // and the restriction is still the engine's rather than a filter in
        // the permission.
        //
        // The registry's status gate is not applied to it, and cannot be: the
        // caller of a cross-tenant read has no tenant to look the registry up
        // by, which is the state the declaration exists for. A suspended store
        // therefore still appears in the list of stores somebody belongs to —
        // which is also how they would learn it is suspended.
        if let Some(subject) = tenancy.cross_tenant_read(&ctx.entry.table, &session.role) {
            return self.subject_compare(ctx, subject, session, path).map(Some);
        }
        let bound = match tenancy.table_scope(&ctx.entry.table) {
            donat_metadata::TableScope::Shared => return Ok(None),
            donat_metadata::TableScope::Key(column) => {
                self.tenant_compare(ctx.info, &ctx.entry.table, column, session, tenant, path)?
            }
            donat_metadata::TableScope::ScopeVia(relationship) => {
                self.tenant_via_relationship(ctx, relationship, session, tenant, path)?
            }
        };
        if !tenant.gated() {
            return Ok(Some(bound));
        }
        Ok(Some(self.with_serving_gate(bound, session, tenant, path)?))
    }

    /// AND the registry's status gate onto a tenant bound.
    ///
    /// A tenant the registry is not serving — suspended, half-provisioned,
    /// closed — must not be answered out of a token that is still valid.
    /// Because the gate is row-independent it lands wherever the bound it is
    /// attached to lands, which means a read returns nothing and a write is
    /// refused by its check. That asymmetry is the same one in-tenant grants
    /// have, and for the same reason: an insert carries a check the database
    /// evaluates, a select carries only a predicate.
    fn with_serving_gate(
        &self,
        bound: BoolExp,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        Ok(BoolExp::And(vec![
            bound,
            self.registry_serving(session, tenant, path)?,
        ]))
    }

    /// The comparison a tenant key column is held to: `= <literal>` for the
    /// session's claim, `= (SELECT <column> FROM <cte> LIMIT 1)` for a step's.
    ///
    /// Every tenant predicate goes through here, so the two arms cannot drift:
    /// a bound that reads the session in one place and the step in another
    /// would be the disagreement the command plane used to refuse outright.
    fn tenant_op(
        &self,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<CompareOp, PlanError> {
        Ok(match tenant {
            TenantRef::Session => CompareOp::Eq(Scalar::Json(serde_json::Value::String(
                self.tenant_value(session, path)?,
            ))),
            TenantRef::Step { cte, column } | TenantRef::Established { cte, column } => {
                CompareOp::CompareStepColumn {
                    cte: cte.clone(),
                    column: column.clone(),
                }
            }
        })
    }

    /// `EXISTS (SELECT 1 FROM <registry> WHERE <key> = <tenant> AND <status>
    /// IN (<serving>))`.
    fn registry_serving(
        &self,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let tenancy = self.tenancy.expect("called only for a tenanted source");
        let registry = &tenancy.registry;
        let Some(info) = self
            .catalog
            .table(registry.table.schema(), registry.table.name())
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!("the tenant registry `{}` does not exist", registry.table),
            ));
        };
        let column_type = |name: &str| -> Result<String, PlanError> {
            info.columns
                .iter()
                .find(|column| column.name == name)
                .map(|column| column.sql_type().to_string())
                .ok_or_else(|| {
                    PlanError::new(
                        path,
                        "unexpected",
                        format!("the registry `{}` has no column `{name}`", registry.table),
                    )
                })
        };
        Ok(BoolExp::Exists {
            table: Table {
                schema: registry.table.schema().to_string(),
                name: registry.table.name().to_string(),
            },
            predicate: Box::new(BoolExp::And(vec![
                BoolExp::Compare {
                    column: registry.key.clone(),
                    pg_type: column_type(&registry.key)?,
                    op: self.tenant_op(session, tenant, path)?,
                },
                BoolExp::Compare {
                    column: registry.status.column.clone(),
                    pg_type: column_type(&registry.status.column)?,
                    op: CompareOp::In(
                        registry
                            .status
                            .serving
                            .iter()
                            .map(|status| Scalar::Json(serde_json::Value::String(status.clone())))
                            .collect(),
                    ),
                },
            ])),
        })
    }

    /// `<subject column> = <the caller>`, for a declared cross-tenant read.
    fn subject_compare(
        &self,
        ctx: &TableCtx,
        subject: &donat_metadata::SubjectBinding,
        session: &Session,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let Some(info_column) = ctx
            .info
            .columns
            .iter()
            .find(|column| column.name == subject.column)
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "table \"{}\" has no column \"{}\" to bound a cross-tenant read by",
                    ctx.entry.table, subject.column
                ),
            ));
        };
        let key = subject.variable.to_ascii_lowercase();
        let Some(value) = session.var(&key).filter(|value| !value.is_empty()) else {
            return Err(PlanError::new(
                path,
                "access-denied",
                format!("this read is bounded by {key}, and the request carries none"),
            ));
        };
        Ok(BoolExp::Compare {
            column: subject.column.clone(),
            pg_type: info_column.sql_type().to_string(),
            op: CompareOp::Eq(Scalar::Json(serde_json::Value::String(value.to_string()))),
        })
    }

    /// `<tenant key> = <the tenant>`, from the session or from a step.
    fn tenant_compare(
        &self,
        info: &donat_catalog_types::TableInfo,
        table: &MetadataTable,
        column: &str,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let Some(info_column) = info.columns.iter().find(|c| c.name == column) else {
            // Deploy-time validation proves this column exists, so reaching
            // here means the database changed under a running snapshot. Fail
            // the request rather than plan a statement with no tenant in it.
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "table \"{table}\" has no tenant key column \"{column}\"; this deployment \
                     cannot be served safely"
                ),
            ));
        };
        Ok(BoolExp::Compare {
            column: column.to_string(),
            pg_type: info_column.sql_type().to_string(),
            op: self.tenant_op(session, tenant, path)?,
        })
    }

    /// A row belonging to several tenants is visible when the caller shares
    /// one with it: `EXISTS (SELECT 1 FROM <link> WHERE <join> AND <link
    /// tenant key> = <caller's tenant>)`.
    ///
    /// This is a correlated traversal rather than an `_exists`, because
    /// `_exists` replaces the predicate context with the remote table entirely
    /// and so cannot say "related to *this* row".
    fn tenant_via_relationship(
        &self,
        ctx: &TableCtx,
        relationship: &str,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let Some((remote_table, join)) = self.relationship_target(ctx, relationship, path) else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "table \"{}\" declares no relationship \"{relationship}\" to scope it by",
                    ctx.entry.table
                ),
            ));
        };
        let Some(remote_info) = self
            .catalog
            .table(remote_table.schema(), remote_table.name())
        else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!("table \"{remote_table}\" does not exist"),
            ));
        };
        let tenancy = self.tenancy.expect("called only for a tenanted source");
        let donat_metadata::TableScope::Key(remote_key) = tenancy.table_scope(&remote_table) else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "table \"{}\" is scoped through \"{relationship}\", but \"{remote_table}\" \
                     carries no tenant key of its own",
                    ctx.entry.table
                ),
            ));
        };
        let predicate = self.tenant_compare(
            remote_info,
            &remote_table,
            remote_key,
            session,
            tenant,
            path,
        )?;
        Ok(BoolExp::Relationship {
            table: Table {
                schema: remote_table.schema().to_string(),
                name: remote_table.name().to_string(),
            },
            join,
            predicate: Box::new(predicate),
        })
    }

    /// The caller's tenant, or the refusal for a request that has none.
    ///
    /// There is no fallback and no default tenant. A request that cannot say
    /// which tenant it is in has no business reading a tenanted table, and
    /// answering it with an empty result instead of an error would hide a
    /// misconfigured token behind a screen that merely looks empty.
    pub(crate) fn tenant_value(&self, session: &Session, path: &str) -> Result<String, PlanError> {
        let tenancy = self.tenancy.expect("called only for a tenanted source");
        let key = tenancy.variable_key();
        match session.var(&key) {
            Some(value) if !value.is_empty() => Ok(value.to_string()),
            _ => Err(PlanError::new(
                path,
                "access-denied",
                format!(
                    "this deployment is tenanted and the request carries no {key}. \
                     A tenant is read from a verified token and from nothing else."
                ),
            )),
        }
    }
}

impl<'a> Planner<'a> {
    /// The tenant predicate a write is bounded by.
    ///
    /// Only a table carrying its own tenant key produces one. A `scope_via`
    /// table deliberately produces none, because a row belonging to several
    /// tenants has no single value a write could be checked against — and the
    /// declaration is only allowed to exist alongside a deploy-time rule that
    /// such a table has no ordinary write permission at all. Shared reference
    /// data likewise has no writers to bound.
    pub(crate) fn write_tenant_predicate(
        &self,
        ctx: &TableCtx,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        self.write_tenant_predicate_from(ctx, session, &TenantRef::Session, path)
    }

    /// [`Self::write_tenant_predicate`], with the tenant taken from wherever
    /// the caller says it lives.
    pub(crate) fn write_tenant_predicate_from(
        &self,
        ctx: &TableCtx,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(tenancy) = self.tenancy else {
            return Ok(None);
        };
        match tenancy.table_scope(&ctx.entry.table) {
            donat_metadata::TableScope::Key(column) => Ok(Some(self.tenant_compare(
                ctx.info,
                &ctx.entry.table,
                column,
                session,
                tenant,
                path,
            )?)),
            _ => Ok(None),
        }
    }

    /// The registry's status gate on its own, for the checks that carry it.
    ///
    /// Kept out of the *predicate* of an update or a delete on purpose. A gate
    /// in a predicate makes a suspended tenant match no rows, which reports a
    /// write that ran and changed nothing — the same wrong answer to "may I"
    /// that a filtered command would give. In a check it raises.
    pub(crate) fn serving_gate(
        &self,
        ctx: &TableCtx,
        session: &Session,
        tenant: &TenantRef,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let Some(tenancy) = self.tenancy else {
            return Ok(None);
        };
        if matches!(
            tenancy.table_scope(&ctx.entry.table),
            donat_metadata::TableScope::Shared
        ) {
            return Ok(None);
        }
        self.registry_serving(session, tenant, path).map(Some)
    }

    /// A write permission's row filter, with the tenant bound added from
    /// wherever this write's tenant lives — or not at all.
    ///
    /// `None` is the write that *creates* the tenant: there is nothing yet to
    /// compare against, and the row being written is the answer. A command
    /// that took its tenant from a step passes that step, and the bound is
    /// then the same predicate a session-scoped write carries, compared to
    /// what the step resolved instead of to the claim.
    pub(crate) fn write_permission_filter_bounded(
        &self,
        filter: &serde_json::Value,
        ctx: &TableCtx<'a>,
        session: &Session,
        tenant: Option<&TenantRef>,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let declared =
            if filter.is_null() || filter.as_object().is_some_and(|object| object.is_empty()) {
                None
            } else {
                let filter_context = self.filter_ctx_of(ctx);
                Some(self.parse_bool_exp(filter, &filter_context, session, true, path)?)
            };
        let bound = match tenant {
            Some(tenant) => self.write_tenant_predicate_from(ctx, session, tenant, path)?,
            None => None,
        };
        Ok(match (declared, bound) {
            (Some(declared), Some(bound)) => Some(BoolExp::And(vec![declared, bound])),
            (Some(declared), None) => Some(declared),
            (None, Some(bound)) => Some(bound),
            (None, None) => None,
        })
    }

    /// A write permission's row filter with the tenant ANDed onto it.
    ///
    /// Reads have one choke point; writes have five — insert, upsert's `DO
    /// UPDATE`, update, delete, and the command plane. They each assemble
    /// their own predicate, so this is the shared piece they all call rather
    /// than five copies of the same `AND`.
    pub(crate) fn write_permission_filter(
        &self,
        filter: &serde_json::Value,
        ctx: &TableCtx<'a>,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let declared =
            if filter.is_null() || filter.as_object().is_some_and(|object| object.is_empty()) {
                None
            } else {
                let filter_context = self.filter_ctx_of(ctx);
                Some(self.parse_bool_exp(filter, &filter_context, session, true, path)?)
            };
        let tenant = self.write_tenant_predicate(ctx, session, path)?;
        Ok(match (declared, tenant) {
            (Some(declared), Some(tenant)) => Some(BoolExp::And(vec![declared, tenant])),
            (Some(declared), None) => Some(declared),
            (None, Some(tenant)) => Some(tenant),
            (None, None) => None,
        })
    }

    /// Everything a write's check carries: what the permission declared, the
    /// tenant bound, the registry's status gate, and the in-tenant grant.
    ///
    /// One builder for all four write kinds, because a check is the only place
    /// an authorization failure can be *reported*. A predicate can only make
    /// rows disappear, and "your update matched nothing" is not an answer to
    /// "may I update this".
    pub(crate) fn write_check_expression(
        &self,
        check: &serde_json::Value,
        ctx: &TableCtx<'a>,
        session: &Session,
        authorization: CheckAuthorization,
        tenant: CheckTenant,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        let mut conjuncts = Vec::new();
        if !check.is_null() && !check.as_object().is_some_and(|object| object.is_empty()) {
            let filter_context = self.filter_ctx_of(ctx);
            conjuncts.push(self.parse_bool_exp(check, &filter_context, session, true, path)?);
        }
        if tenant.repeats_the_bound()
            && let Some(bound) = self.write_tenant_predicate(ctx, session, path)?
        {
            conjuncts.push(bound);
        }
        // The gate is read by whichever tenant this write has — the session's
        // claim, or the value a command step resolved. The one write with no
        // gate is the one that *creates* the tenant: it is writing the
        // registry row the gate would read.
        if let Some(gate_tenant) = tenant.gate_tenant()
            && let Some(gate) = self.serving_gate(ctx, session, &gate_tenant, path)?
        {
            conjuncts.push(gate);
        }
        let declared = match conjuncts.len() {
            0 => None,
            1 => conjuncts.pop(),
            _ => Some(BoolExp::And(conjuncts)),
        };
        match authorization {
            CheckAuthorization::Table(operation) => {
                self.with_iam(declared, ctx, session, operation, path)
            }
            // A command is authorized once, by its own action, so the table's
            // action is not applied again inside it — that would mean two
            // grants for one operation. The reservation is not the table's
            // action: it is a property of the row, and it must hold however
            // the row arrives, or a deployment that writes its grants from a
            // command can grant itself anything the platform reserved.
            CheckAuthorization::CommandStep(operation) => {
                let mut conjuncts = Vec::new();
                if let Some(declared) = declared {
                    conjuncts.push(declared);
                }
                if matches!(
                    operation,
                    donat_metadata::IamOperation::Insert | donat_metadata::IamOperation::Update
                ) && let Some(reserved) = self.reserved_action_bound(ctx, path)?
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
    }

    /// The tenant column a write must carry, as `(column, pg_type, value)`.
    ///
    /// This is injected as an ordinary permission preset, which means it
    /// overrides whatever the caller supplied. That is the point: a client may
    /// name another tenant's id in an insert object and the value is simply
    /// replaced, so the attempt does not fail — it lands in the caller's own
    /// tenant, which is the same thing that happens today when a permission
    /// presets `user_id`.
    pub(crate) fn tenant_preset(
        &self,
        ctx: &TableCtx,
        session: &Session,
        path: &str,
    ) -> Result<Option<(String, String, String)>, PlanError> {
        let Some(tenancy) = self.tenancy else {
            return Ok(None);
        };
        let donat_metadata::TableScope::Key(column) = tenancy.table_scope(&ctx.entry.table) else {
            return Ok(None);
        };
        let Some(info_column) = ctx.info.columns.iter().find(|c| c.name == column) else {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!(
                    "table \"{}\" has no tenant key column \"{column}\"; this deployment cannot \
                     be served safely",
                    ctx.entry.table
                ),
            ));
        };
        Ok(Some((
            column.to_string(),
            info_column.sql_type().to_string(),
            self.tenant_value(session, path)?,
        )))
    }
}
