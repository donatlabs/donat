//! An unbounded permission says so.
//!
//! A permission that does not bound its rows to the caller is written `filter:
//! {}` — and so is a permission whose author forgot the bound. The two are
//! indistinguishable, which is why a role reading every other seller's orders
//! survived review in the Petshop example: nothing about the metadata said
//! whether the absence was a decision or an oversight.
//!
//! The tenant solved the same problem by moving the bound into the compiler,
//! because a tenant is one value on one column, identical on every table and
//! for every role. Ownership is not: the path to the owner differs per table,
//! and whether a table has an owner at all differs per role — a marketplace's
//! catalogue is meant to be read by every shopper, and the same table read by
//! a seller is not. There is no rule to declare once, so the rule stays where
//! it can vary, in the permission's own `filter`.
//!
//! What can be made uniform is the *guarantee*, and this file is that: a
//! deployment sets `unbounded_permissions: declared` and every permission that
//! grants rows it does not bound to the caller must name the reason. Nothing
//! is injected and nothing is inferred — the author writes it, and a reviewer
//! reads it. Forgetting stops being invisible, which is the property tenancy
//! has and permissions did not.
//!
//! Two rules, both deploy-time:
//!
//! *An unbounded permission with no reason refuses the deployment*, naming the
//! table, the role and the operation — but only where the deployment asked for
//! the check, because metadata exported from an existing Donat project has
//! never heard of it and must still load.
//!
//! *A reason on a permission that does bind its caller refuses too*, always.
//! A declaration the runtime ignores is a defect
//! (`knowledgebase/declarative-saas/decisions/034-*`), and a stale `unbounded:`
//! is worse than none: it tells a reviewer a bound was considered and declined
//! on a permission where one is in fact present.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::types::{BoolExp, Metadata, PermissionEntry, QualifiedTable};

/// Why a permission grants rows it does not bound to the caller.
///
/// The three answers are the ones the Petshop example actually gives, read off
/// its 155 unbounded permissions rather than invented: the rows are nobody's in
/// particular, the caller is a desk that is supposed to see everyone's, or
/// there is no caller at all. A free-text reason was rejected — it is
/// unvalidatable and copy-pasted; an enum makes the author choose between
/// answers a reviewer can disagree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnboundedReason {
    /// The rows belong to nobody in particular and every caller of this role
    /// is meant to see the same ones — a product catalogue, a price list, a
    /// list of shipping methods.
    Catalogue,
    /// The role is a desk rather than a person's own data: support, back
    /// office, fulfilment. Seeing every customer's row is the job, and the
    /// bound that matters is who may hold the role at all.
    Operator,
    /// A fixed role a process or a worker runs as. There is no session to
    /// bound against, so a caller bound cannot be written here even in
    /// principle — `donat validate` refuses one that tries.
    Worker,
    /// A command permission whose row is chosen by the command's own step
    /// rather than by the caller (ADR-019). Only legal on the four
    /// `command_*_permissions` planes, because on the ordinary planes there is
    /// no command to do the bounding and the claim would be false.
    ///
    /// The claim is checked rather than trusted. A permission is only allowed
    /// to make it where no generic root can reach it: on a `command_*` plane,
    /// which schema generation ignores entirely, or on an ordinary plane of a
    /// table the role has no ordinary `select_permissions` on — because a role
    /// without one never sees the table in its schema, and so never gets an
    /// insert, update or delete root for it either. Anywhere else the claim is
    /// false and the deployment is refused.
    ///
    /// What stays a reviewer's job is the half the engine cannot see: that the
    /// step's `by` reaches only rows the caller is entitled to. That is the
    /// same audited shape `unscoped_steps: audited` has in `tenancy.yaml`.
    Command,
}

impl UnboundedReason {
    /// The spelling used in messages, matching what the author wrote.
    pub fn as_str(self) -> &'static str {
        match self {
            UnboundedReason::Catalogue => "catalogue",
            UnboundedReason::Operator => "operator",
            UnboundedReason::Worker => "worker",
            UnboundedReason::Command => "command",
        }
    }
}

/// Whether this deployment requires unbounded permissions to declare a reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnboundedPolicy {
    /// The default, and what unconverted v2 metadata gets: an absent
    /// `unbounded:` is accepted. A deployment that has never made the pass
    /// keeps loading.
    #[default]
    Unchecked,
    /// Every permission that does not bound its rows to the caller must name a
    /// reason, or the deployment refuses to start.
    Declared,
}

/// The optional `permissions.yaml` section.
///
/// Deliberately its own file rather than a field of `tenancy.yaml`: the two
/// answer different questions, and a deployment with one tenant wants this
/// check exactly as much as a deployment with a thousand. Petshop is not
/// tenanted and is the reason the check exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsMetadata {
    #[serde(default)]
    pub unbounded_permissions: UnboundedPolicy,
}

impl PermissionsMetadata {
    /// Serialising the default would write a `permissions.yaml` into every
    /// export that had never asked for one.
    pub fn is_default(&self) -> bool {
        *self == PermissionsMetadata::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundsError {
    pub path: String,
    pub message: String,
}

impl BoundsError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BoundsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

fn is_session_variable(name: &str) -> bool {
    name.get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-donat-"))
        || name
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-hasura-"))
}

/// Does this expression bound the rows it admits to the caller?
///
/// The question is not "is the filter non-empty" — `{status: {_eq: 'paid'}}`
/// is non-empty and every caller of the role still sees the same rows. It is
/// whether reaching TRUE requires a fact about *who is asking*, which is what
/// a session variable is.
///
/// Answered conservatively, so the analysis can be wrong only in the direction
/// of asking the author for a declaration they did not owe:
///
/// | Shape | Binds |
/// |---|---|
/// | `{}` | no — the empty filter admits every row |
/// | `{col: {_eq: X-Donat-User-Id}}` | yes |
/// | `{col: {_eq: 'literal'}}` | no |
/// | `{col: {_in: [X-Donat-Org-Id]}}` | yes — a session value anywhere in the operand |
/// | `{_and: [...]}`, or several keys in one object | yes if **any** arm binds |
/// | `{_or: [...]}` | yes only if **every** arm binds, and there is at least one |
/// | `{_not: ...}` | no — negating a bound does not bound |
/// | `{rel: {...}}`, `{_exists: {_where: {...}}}` | whatever the inner expression answers |
///
/// The `_or` row is the one that earns its keep: `{_or: [{owner: {_eq:
/// X-Donat-User-Id}}, {}]}` mentions a session variable and admits every row
/// anyway, and a rule that merely grepped for `x-donat-` would pass it.
///
/// `tenant_variable` names the variable a tenanted deployment scopes by, and
/// it is deliberately *not* a caller bound. A permission bounded only by tenant
/// admits every row of that tenant — every seller's order in one marketplace —
/// which is precisely the case this whole file exists to make visible.
pub fn binds_caller(expr: &BoolExp, tenant_variable: Option<&str>) -> bool {
    fn session_value(value: &serde_json::Value, tenant: Option<&str>) -> bool {
        match value {
            serde_json::Value::String(s) => {
                is_session_variable(s)
                    && !tenant.is_some_and(|tenant| tenant.eq_ignore_ascii_case(s))
            }
            serde_json::Value::Array(items) => items.iter().any(|item| session_value(item, tenant)),
            _ => false,
        }
    }

    fn walk(expr: &serde_json::Value, tenant: Option<&str>) -> bool {
        let serde_json::Value::Object(map) = expr else {
            return false;
        };
        // Several keys in one object are an implicit AND, so any bound arm
        // bounds the whole — the same rule `_and` gets below.
        map.iter().any(|(key, value)| match key.as_str() {
            "_and" | "$and" => match value {
                serde_json::Value::Array(items) => items.iter().any(|item| walk(item, tenant)),
                other => walk(other, tenant),
            },
            "_or" | "$or" => match value {
                serde_json::Value::Array(items) => {
                    !items.is_empty() && items.iter().all(|item| walk(item, tenant))
                }
                other => walk(other, tenant),
            },
            // A bound inside a negation is not a bound: `_not` over
            // `owner = me` admits everybody else's rows.
            "_not" | "$not" => false,
            "_exists" => value.get("_where").is_some_and(|inner| walk(inner, tenant)),
            // A comparison operator: bound iff the operand names the session.
            // Anything else is a column or a relationship, and the answer is
            // whatever its own expression gives.
            key if key.starts_with('_') || key.starts_with('$') => session_value(value, tenant),
            _ => walk(value, tenant),
        })
    }

    walk(expr, tenant_variable)
}

/// One permission's worth of what this file checks.
struct Bounded<'a> {
    role: &'a str,
    expression: &'a BoolExp,
    unbounded: Option<UnboundedReason>,
}

fn entries<'a, P: 'a>(
    list: &'a [PermissionEntry<P>],
    read: impl Fn(&'a P) -> (&'a BoolExp, Option<UnboundedReason>) + 'a,
) -> impl Iterator<Item = Bounded<'a>> + 'a {
    list.iter().map(move |entry| {
        let (expression, unbounded) = read(&entry.permission);
        Bounded {
            role: &entry.role,
            expression,
            unbounded,
        }
    })
}

fn table_name(table: &QualifiedTable) -> String {
    format!("{}.{}", table.schema(), table.name())
}

/// Can a generic GraphQL/REST/MCP root reach this table as this role?
///
/// Schema generation gives a role a table only when it has an ordinary select
/// permission on it (`Planner::table_ctx` returns `None` otherwise), and every
/// insert, update and delete root is emitted inside that same block. So a role
/// with no ordinary select has no generic root on the table at all, whatever
/// its write permissions say — which is what makes an unbounded `check: {}`
/// there reachable only through a command step.
///
/// Inheritance is walked the whole way, with a visited set, because that is what
/// the runtime does: `Planner::role_select_perms_from` recurses through each
/// parent's own parents, so `A -> B -> C` gives `A` the table when only `C`
/// declares the select. Stopping at one hop answered "no root" for a role that
/// has one — and that is the one direction this analysis must never be wrong
/// in, because it *accepts* a false `unbounded: command` instead of refusing a
/// true one.
fn generic_roots_reach(metadata: &Metadata, table: &crate::types::TableEntry, role: &str) -> bool {
    fn walk(
        metadata: &Metadata,
        table: &crate::types::TableEntry,
        role: &str,
        visiting: &mut BTreeSet<String>,
    ) -> bool {
        if table
            .select_permissions
            .iter()
            .any(|entry| entry.role == role)
        {
            return true;
        }
        // A cycle in the declaration is somebody else's error to report; here
        // it just must not loop.
        if !visiting.insert(role.to_owned()) {
            return false;
        }
        metadata
            .inherited_roles
            .iter()
            .find(|inherited| inherited.role_name == role)
            .is_some_and(|inherited| {
                inherited
                    .role_set
                    .iter()
                    .any(|parent| walk(metadata, table, parent, visiting))
            })
    }

    walk(metadata, table, role, &mut BTreeSet::new())
}

/// Every bounds rule that can be decided from metadata alone.
pub fn validate_permission_bounds(metadata: &Metadata) -> Vec<BoundsError> {
    let required = matches!(
        metadata.permissions.unbounded_permissions,
        UnboundedPolicy::Declared
    );
    let tenant_variable = metadata
        .tenancy
        .as_ref()
        .map(|tenancy| tenancy.variable.as_str());
    let mut errors = Vec::new();

    for source in &metadata.sources {
        for table in &source.tables {
            let name = table_name(&table.table);
            let planes: [(&str, Box<dyn Iterator<Item = Bounded<'_>>>); 8] = [
                (
                    "select_permissions",
                    Box::new(entries(&table.select_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
                (
                    "insert_permissions",
                    Box::new(entries(&table.insert_permissions, |p| {
                        (&p.check, p.unbounded)
                    })),
                ),
                (
                    "update_permissions",
                    Box::new(entries(&table.update_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
                (
                    "delete_permissions",
                    Box::new(entries(&table.delete_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
                (
                    "command_select_permissions",
                    Box::new(entries(&table.command_select_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
                (
                    "command_insert_permissions",
                    Box::new(entries(&table.command_insert_permissions, |p| {
                        (&p.check, p.unbounded)
                    })),
                ),
                (
                    "command_update_permissions",
                    Box::new(entries(&table.command_update_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
                (
                    "command_delete_permissions",
                    Box::new(entries(&table.command_delete_permissions, |p| {
                        (&p.filter, p.unbounded)
                    })),
                ),
            ];

            for (plane, permissions) in planes {
                let command_plane = plane.starts_with("command_");
                for permission in permissions {
                    let path = format!("{name}.{plane}[{}]", permission.role);
                    let binds = binds_caller(permission.expression, tenant_variable);
                    // `command` claims the bound lives in a command step. It
                    // holds only where nothing but a command can arrive.
                    if permission.unbounded == Some(UnboundedReason::Command)
                        && !command_plane
                        && generic_roots_reach(metadata, table, permission.role)
                    {
                        errors.push(BoundsError::new(
                            path.clone(),
                            format!(
                                "declares `unbounded: command`, but {} has an ordinary select \
                                 permission on this table, so generic roots reach this \
                                 permission and no command step bounds them",
                                permission.role
                            ),
                        ));
                        continue;
                    }
                    match (permission.unbounded, binds) {
                        (Some(reason), true) => errors.push(BoundsError::new(
                            path,
                            format!(
                                "declares `unbounded: {}` but its expression does bound the \
                                 caller, so the declaration is false; remove it",
                                reason.as_str()
                            ),
                        )),
                        (None, false) if required => errors.push(BoundsError::new(
                            path,
                            if command_plane
                                || !generic_roots_reach(metadata, table, permission.role)
                            {
                                "admits rows it does not bound to the caller and does not say \
                                 why; add `unbounded:` naming one of catalogue, operator, worker \
                                 or command, or bound the rows"
                            } else {
                                "admits rows it does not bound to the caller and does not say \
                                 why; add `unbounded:` naming one of catalogue, operator or \
                                 worker, or bound the rows"
                            }
                            .to_string(),
                        )),
                        _ => {}
                    }
                }
            }
        }
    }

    errors
}
