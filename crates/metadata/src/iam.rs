//! In-tenant authorization: grants a tenant writes for itself, compiled into
//! the predicates its roles are served through.
//!
//! There are two layers here and keeping them apart is the design.
//!
//! A **compiled role** decides the *shape*: which tables, columns and
//! operations exist at all. It lives in metadata, in git, and changes by
//! deploying. A tenant cannot invent one, because inventing one would mean
//! compiling a new GraphQL schema per tenant.
//!
//! A **grant** decides the *scope* within that shape: which actions the caller
//! holds. It is rows in the tenant's own tables, unbounded in number,
//! different per tenant, and changed at runtime by the tenant itself.
//!
//! The same behaviour is expressible today without this file — an `_exists`
//! against a grant table, repeated in the filter and the check of every
//! permission of every table. That is the state of the art elsewhere, and it
//! is exactly what stops scaling at a hundred and sixty tables: the predicate
//! is written by hand as many times as there are permissions, and a missing
//! one is invisible.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::tenancy::same_table;
use crate::types::{Metadata, QualifiedTable};

/// The flattened grant relation and how to read a caller out of it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRelation {
    /// One row per (tenant, subject, action). Role hierarchy is expanded into
    /// it by the database — a view, a materialized one, or a table something
    /// maintains — so the predicate never walks a hierarchy at query time.
    pub table: QualifiedTable,
    pub subject: crate::tenancy::SubjectBinding,
    pub tenant: crate::tenancy::ColumnBinding,
    pub action: crate::tenancy::ColumnBinding,
    /// The table a tenant's own writes land in, and the column holding the
    /// action string. Required before `reserved_actions` can mean anything:
    /// without somewhere to enforce them, a reserved action is a promise with
    /// no gate behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_via: Option<GrantWriteTarget>,
}

/// Where a tenant writes its own grants.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrantWriteTarget {
    pub table: QualifiedTable,
    pub action: String,
}

/// How a required action is spelled for a table operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTemplates {
    #[serde(default = "default_select")]
    pub select: String,
    #[serde(default = "default_insert")]
    pub insert: String,
    #[serde(default = "default_update")]
    pub update: String,
    #[serde(default = "default_delete")]
    pub delete: String,
}

fn default_select() -> String {
    "{table}:read".to_string()
}
fn default_insert() -> String {
    "{table}:create".to_string()
}
fn default_update() -> String {
    "{table}:update".to_string()
}
fn default_delete() -> String {
    "{table}:delete".to_string()
}

impl Default for ActionTemplates {
    fn default() -> Self {
        Self {
            select: default_select(),
            insert: default_insert(),
            update: default_update(),
            delete: default_delete(),
        }
    }
}

/// Several tables that are one resource to the people using them. An order and
/// its lines are one thing a merchant grants access to, not two.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOverride {
    pub tables: Vec<QualifiedTable>,
    pub resource: String,
}

/// Table x operation -> required action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionMapping {
    #[serde(default)]
    pub default: ActionTemplates,
    /// The default derives the action from the table name, so a new table is
    /// governed the moment it is tracked. An override groups several tables
    /// under one business resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<ResourceOverride>,
}

/// What one operation on one table requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IamOperation {
    Select,
    Insert,
    Update,
    Delete,
}

/// In-tenant authorization for one source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IamMetadata {
    pub source: String,
    pub grants: GrantRelation,
    /// Which compiled roles are governed by grants at all.
    ///
    /// A storefront shopper is tenant-scoped and holds no grants; forcing that
    /// role through the grant relation would deny every request it makes. The
    /// list is explicit rather than inferred because "this role is not
    /// governed" is a decision, and an inferred one is a decision nobody made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub governed_roles: Vec<String>,
    /// `service:action`, AWS-style. A grant matches a required action directly
    /// or through one of these expansions.
    #[serde(default = "default_wildcards", skip_serializing_if = "Vec::is_empty")]
    pub wildcards: Vec<String>,
    #[serde(default)]
    pub actions: ActionMapping,
    /// Command name -> required action, so a role can be allowed to read
    /// orders but not to cancel them. The command's own `permissions:` list
    /// stays the outer gate; this is an additional, narrower one.
    #[serde(default)]
    pub command_actions: CommandActionMapping,
    /// Actions a tenant's own roles may never hold, whatever it writes into
    /// its grant rows. These belong to the platform.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_actions: Vec<String>,
}

fn default_wildcards() -> Vec<String> {
    vec![
        "{resource}:*".to_string(),
        "*:{verb}".to_string(),
        "*:*".to_string(),
    ]
}

/// Command -> required action.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandActionMapping {
    #[serde(default = "default_command_action")]
    pub default: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<CommandActionOverride>,
}

fn default_command_action() -> String {
    "{command}:invoke".to_string()
}

impl Default for CommandActionMapping {
    fn default() -> Self {
        Self {
            default: default_command_action(),
            overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandActionOverride {
    pub command: String,
    pub action: String,
}

impl IamMetadata {
    /// Every action string that authorizes invoking this command.
    pub fn accepted_command_actions(&self, command: &str) -> Vec<String> {
        let exact = self
            .command_actions
            .overrides
            .iter()
            .find(|entry| entry.command == command)
            .map(|entry| entry.action.clone())
            .unwrap_or_else(|| self.command_actions.default.replace("{command}", command));
        let (resource, verb) = exact
            .split_once(':')
            .map(|(resource, verb)| (resource.to_string(), verb.to_string()))
            .unwrap_or_else(|| (exact.clone(), String::new()));
        let mut accepted = BTreeSet::new();
        accepted.insert(exact);
        for wildcard in &self.wildcards {
            accepted.insert(
                wildcard
                    .replace("{resource}", &resource)
                    .replace("{table}", &resource)
                    .replace("{command}", &resource)
                    .replace("{verb}", &verb),
            );
        }
        accepted.into_iter().collect()
    }

    /// Is this compiled role served through the grant relation?
    pub fn governs(&self, role: &str) -> bool {
        self.governed_roles.iter().any(|governed| governed == role)
    }

    /// The resource name a table is granted under: its own name, or the one an
    /// override groups it into.
    pub fn resource_for(&self, table: &QualifiedTable) -> String {
        self.actions
            .overrides
            .iter()
            .find(|entry| {
                entry
                    .tables
                    .iter()
                    .any(|candidate| same_table(candidate, table))
            })
            .map(|entry| entry.resource.clone())
            .unwrap_or_else(|| table.name().to_string())
    }

    /// Every action string that satisfies this operation on this table: the
    /// exact one, plus each declared wildcard expanded for it.
    ///
    /// Expanding here rather than matching with `LIKE` at query time is
    /// deliberate — a grant row is compared for equality against a short list,
    /// which an index answers, and no pattern a tenant wrote is ever executed.
    pub fn accepted_actions(&self, table: &QualifiedTable, operation: IamOperation) -> Vec<String> {
        let resource = self.resource_for(table);
        let template = match operation {
            IamOperation::Select => &self.actions.default.select,
            IamOperation::Insert => &self.actions.default.insert,
            IamOperation::Update => &self.actions.default.update,
            IamOperation::Delete => &self.actions.default.delete,
        };
        let exact = template
            .replace("{table}", &resource)
            .replace("{resource}", &resource);
        let verb = exact
            .split_once(':')
            .map(|(_, verb)| verb.to_string())
            .unwrap_or_default();
        let mut accepted = BTreeSet::new();
        accepted.insert(exact);
        for wildcard in &self.wildcards {
            accepted.insert(
                wildcard
                    .replace("{resource}", &resource)
                    .replace("{table}", &resource)
                    .replace("{verb}", &verb),
            );
        }
        accepted.into_iter().collect()
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One refusal, naming the metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IamDeclarationError {
    pub path: String,
    pub message: String,
}

impl IamDeclarationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IamDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// Every IAM rule decidable from metadata alone.
pub fn validate_iam_declaration(metadata: &Metadata) -> Vec<IamDeclarationError> {
    let Some(iam) = &metadata.iam else {
        return Vec::new();
    };
    let mut errors = Vec::new();

    // Grants are read per tenant, so a source without tenancy has no tenant
    // column for the relation to be keyed by, and the whole layer would grant
    // across every tenant at once.
    match &metadata.tenancy {
        Some(tenancy) if tenancy.source == iam.source => {}
        _ => errors.push(IamDeclarationError::new(
            "iam.source",
            format!(
                "source `{}` declares grants but no tenancy; a grant is held in one tenant, and \
                 without one the relation would answer for every tenant at once",
                iam.source
            ),
        )),
    }

    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == iam.source)
    else {
        errors.push(IamDeclarationError::new(
            "iam.source",
            format!("source `{}` is not declared in databases.yaml", iam.source),
        ));
        return errors;
    };

    let tracked = |table: &QualifiedTable| {
        source
            .tables
            .iter()
            .any(|entry| same_table(&entry.table, table))
    };

    if !tracked(&iam.grants.table) {
        errors.push(IamDeclarationError::new(
            "iam.grants.table",
            format!(
                "`{}` is not tracked in source `{}`, so no permission decides who may read it",
                iam.grants.table, iam.source
            ),
        ));
    }
    if !is_session_variable(&iam.grants.subject.variable) {
        errors.push(IamDeclarationError::new(
            "iam.grants.subject.variable",
            format!(
                "'{}' is not a session variable name",
                iam.grants.subject.variable
            ),
        ));
    }

    if !iam.reserved_actions.is_empty() && iam.grants.written_via.is_none() {
        errors.push(IamDeclarationError::new(
            "iam.reserved_actions",
            "actions are reserved but `grants.written_via` does not say where a tenant writes \
             its grants, so nothing would enforce the reservation"
                .to_string(),
        ));
    }
    if let Some(target) = &iam.grants.written_via
        && !tracked(&target.table)
    {
        errors.push(IamDeclarationError::new(
            "iam.grants.written_via.table",
            format!(
                "`{}` is not tracked in source `{}`",
                target.table, iam.source
            ),
        ));
    }

    for (index, entry) in iam.actions.overrides.iter().enumerate() {
        let path = format!("iam.actions.overrides[{index}]");
        if entry.tables.is_empty() {
            errors.push(IamDeclarationError::new(
                &path,
                format!("resource `{}` groups no tables", entry.resource),
            ));
        }
        for table in &entry.tables {
            if !tracked(table) {
                errors.push(IamDeclarationError::new(
                    &path,
                    format!("`{table}` is not tracked in source `{}`", iam.source),
                ));
            }
        }
    }

    // A governed role that exists nowhere is a rule that will never fire, and
    // a rule that never fires reads as protection that is not there.
    let declared_roles = declared_roles(metadata, &iam.source);
    for (index, role) in iam.governed_roles.iter().enumerate() {
        if !declared_roles.contains(role) {
            errors.push(IamDeclarationError::new(
                format!("iam.governed_roles[{index}]"),
                format!("role `{role}` holds no permission anywhere in this source"),
            ));
        }
    }

    errors
}

fn is_session_variable(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

fn declared_roles(metadata: &Metadata, source_name: &str) -> BTreeSet<String> {
    let mut roles = BTreeSet::new();
    for source in metadata.sources.iter().filter(|s| s.name == source_name) {
        for entry in &source.tables {
            for role in entry
                .select_permissions
                .iter()
                .map(|p| &p.role)
                .chain(entry.insert_permissions.iter().map(|p| &p.role))
                .chain(entry.update_permissions.iter().map(|p| &p.role))
                .chain(entry.delete_permissions.iter().map(|p| &p.role))
                .chain(entry.command_select_permissions.iter().map(|p| &p.role))
                .chain(entry.command_insert_permissions.iter().map(|p| &p.role))
                .chain(entry.command_update_permissions.iter().map(|p| &p.role))
                .chain(entry.command_delete_permissions.iter().map(|p| &p.role))
            {
                roles.insert(role.clone());
            }
        }
    }
    for command in metadata.commands.iter().filter(|c| c.source == source_name) {
        for permission in &command.permissions {
            roles.insert(permission.role.clone());
        }
    }
    roles
}
