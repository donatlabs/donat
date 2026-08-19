//! Tenancy as a compiler layer, declared once in `tenancy.yaml`.
//!
//! A tenant is not a filter somebody remembered to write. Every deployment
//! that hand-rolls one repeats the same predicate in the `filter` and the
//! `check` of every permission of every table, and the inventions differ in
//! ways that only show up as a leak. This file is the declaration that lets
//! the compiler do it instead: the tenant predicate is ANDed into every
//! permission and the tenant preset is injected into every write, so a table
//! is scoped because it was tracked, not because it was remembered.
//!
//! Two properties are the whole point, and both are deploy-time:
//!
//! *Forgetting a table is a boot failure.* Every tracked table in a tenanted
//! source either carries the tenant key, or says why it does not — under
//! `keys:` because its key has another name, or under `exempt:` because it
//! genuinely belongs to no single tenant. A table that says neither stops the
//! deployment, naming itself. Forgetting the 158th table must be impossible,
//! not merely unlikely.
//!
//! *The tenant is a claim, and never a header.* It reaches the engine the way
//! a role does — from a verified token, or from an authentication hook — and
//! from nothing else. Unlike `X-Donat-Role`, which selects among roles a token
//! already granted, there is nothing for a tenant header to select among: the
//! claim is the single value. So no header names one, and there is no shared
//! secret that lets a request assert its own tenant, because there is no
//! shared secret at all (`knowledgebase/api-surfaces/decisions/013-*`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::types::{Metadata, QualifiedTable, Source, SourceKind};

/// How one tenant's rows are separated from every other tenant's.
///
/// Only `row_key` exists. The other two bindings a platform eventually wants —
/// a schema per tenant, a database per tenant — are named in the design and
/// deliberately absent from this enum, because a declaration the runtime
/// ignores is a defect (`knowledgebase/declarative-saas/decisions/034-*`). The
/// field exists so that adding one stays a single declaration change rather
/// than a rewrite of the data model, the permissions and the commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TenancyBinding {
    /// One database, one schema, a tenant column on every table.
    #[default]
    RowKey,
}

/// Where the tenant value is allowed to come from.
///
/// One variant, for the same reason the role has one: anything else would be a
/// request asserting its own tenant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TenancyTrust {
    /// Read only from a verified token, or from an authentication hook's
    /// response — the two mechanisms that establish a role.
    #[default]
    JwtClaim,
}

/// What a command's steps may do outside the tenant scope.
///
/// The default is the closed one. Opening the escape hatch is a thing a
/// deployment writes down, because every use of it is a read that the tenant
/// predicate did not cover and the only remaining authorization is that the
/// lookup key is unguessable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnscopedStepPolicy {
    /// No step may declare `tenant: unscoped`.
    #[default]
    Forbidden,
    /// A step may, under the conditions the compiler enforces, and every one
    /// of them is listed by `donat validate`.
    Audited,
}

/// The tenant registry: the table that decides which tenants exist and which
/// of them are currently served.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TenantRegistry {
    pub table: QualifiedTable,
    /// The column holding the tenant's own identifier — the value every other
    /// table's tenant key points at.
    pub key: String,
    pub status: TenantStatusGate,
}

/// Which registry rows are served. A tenant that is not serving is refused
/// before its rows are ever considered, rather than filtered out afterwards:
/// a suspended store must fail loudly, not look empty.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TenantStatusGate {
    pub column: String,
    pub serving: Vec<String>,
}

/// A table whose tenant key is spelled differently.
///
/// This is not an exemption. These rows still belong to exactly one tenant,
/// and keeping the distinction in the syntax is deliberate — putting the
/// registry under `exempt:` instead would let any member of any tenant read
/// every tenant there is.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TenantKeyOverride {
    pub table: QualifiedTable,
    pub key: String,
}

/// A table that genuinely has no one owning tenant.
///
/// Exactly one of `shared` and `scope_via` must be present. A table with
/// neither would be an unscoped table every tenant can write, which is a side
/// channel between tenants however innocent the rows look.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TenantExemption {
    pub table: QualifiedTable,
    /// Platform-owned reference data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedAccess>,
    /// A row belonging to several tenants, visible through the intersection of
    /// memberships: this names an array relationship on the table, and the
    /// compiler turns it into a correlated traversal. `_exists` cannot express
    /// it, because `_exists` switches the predicate context to the remote
    /// table entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_via: Option<String>,
}

/// How widely a shared table may be reached. There is one answer: a shared
/// *writable* table is a channel from one tenant to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedAccess {
    ReadOnly,
}

/// One read that is bounded by the caller rather than by their tenant.
///
/// This exists because of a hole the strict rule would otherwise leave: a
/// person who has just signed in belongs to some set of tenants and is
/// currently in none of them. Without a way to read that set, a store switcher
/// cannot be built, and the usual workaround — a role with a wider filter — is
/// exactly the hand-rolled cross-tenant view the design set out to avoid.
///
/// So the engine substitutes rather than relaxes: for this table and this role
/// the tenant predicate is replaced by `<column> = <the caller>`. The row is
/// still bounded, by subject instead of by tenant, and the bound is the
/// engine's rather than a filter somebody wrote. It applies to reads only —
/// a role that may look across tenants may still never write across them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrossTenantRead {
    pub table: QualifiedTable,
    pub role: String,
    pub scoped_by: SubjectBinding,
}

/// A column, and the session variable its value is compared against.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectBinding {
    pub column: String,
    pub variable: String,
}

/// A column named on a relation the engine reads for itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnBinding {
    pub column: String,
}

/// The whole tenancy surface of one source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TenancyMetadata {
    /// Which source is tenanted. One, in wave 1: a second tenanted source
    /// would need a story for a mutation that spans both, and mutations may
    /// already target only one source.
    pub source: String,
    #[serde(default)]
    pub binding: TenancyBinding,
    /// The session variable carrying the tenant.
    pub variable: String,
    #[serde(default)]
    pub trust: TenancyTrust,
    /// The tenant column every tracked table carries unless it says otherwise.
    pub key: String,
    pub registry: TenantRegistry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<TenantKeyOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt: Vec<TenantExemption>,
    #[serde(default)]
    pub unscoped_steps: UnscopedStepPolicy,
    /// Reads bounded by the caller instead of by their tenant. Every entry is
    /// a hole in the default rule, so they are listed here, in one place, and
    /// `donat validate` prints them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_tenant_reads: Vec<CrossTenantRead>,
}

/// How one tracked table is scoped. This is what every downstream consumer
/// asks for; nobody re-walks `keys:` and `exempt:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableScope<'a> {
    /// Scoped by a tenant column of this name.
    Key(&'a str),
    /// Platform reference data, readable by everyone and writable by no
    /// tenant-facing role.
    Shared,
    /// Scoped by traversing an array relationship of this name.
    ScopeVia(&'a str),
}

impl TenancyMetadata {
    /// The lower-cased session variable name, which is how a [`Session`]
    /// stores it.
    ///
    /// [`Session`]: https://docs.rs/donat-schema
    pub fn variable_key(&self) -> String {
        self.variable.to_ascii_lowercase()
    }

    /// How a table in the tenanted source is scoped.
    ///
    /// A table named nowhere carries the default key — which is the rule that
    /// makes tracking a table enough to scope it, and the reason the catalog
    /// check that the column really exists is not optional.
    pub fn table_scope(&self, table: &QualifiedTable) -> TableScope<'_> {
        if let Some(entry) = self
            .keys
            .iter()
            .find(|entry| same_table(&entry.table, table))
        {
            return TableScope::Key(&entry.key);
        }
        if let Some(entry) = self
            .exempt
            .iter()
            .find(|entry| same_table(&entry.table, table))
        {
            if let Some(relationship) = &entry.scope_via {
                return TableScope::ScopeVia(relationship);
            }
            return TableScope::Shared;
        }
        TableScope::Key(&self.key)
    }

    /// Is this table platform-owned reference data?
    pub fn is_shared(&self, table: &QualifiedTable) -> bool {
        matches!(self.table_scope(table), TableScope::Shared)
    }

    /// The subject bound that replaces the tenant predicate for this table and
    /// role, if one was declared.
    pub fn cross_tenant_read(&self, table: &QualifiedTable, role: &str) -> Option<&SubjectBinding> {
        self.cross_tenant_reads
            .iter()
            .find(|entry| entry.role == role && same_table(&entry.table, table))
            .map(|entry| &entry.scoped_by)
    }
}

/// Two table references naming the same table. `QualifiedTable` has three
/// spellings and an unqualified name means `public`, so equality on the enum
/// is not the question being asked.
pub(crate) fn same_table(left: &QualifiedTable, right: &QualifiedTable) -> bool {
    left.schema() == right.schema() && left.name() == right.name()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One refusal, naming the metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenancyDeclarationError {
    pub path: String,
    pub message: String,
}

impl TenancyDeclarationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TenancyDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

fn is_session_variable(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

/// Every tenancy rule that can be decided from metadata alone.
///
/// The one rule that cannot is whether each table's tenant column actually
/// exists in the database; that is checked where a catalog is in hand, and it
/// is the reason `donat validate` introspects.
pub fn validate_tenancy_declaration(metadata: &Metadata) -> Vec<TenancyDeclarationError> {
    let Some(tenancy) = &metadata.tenancy else {
        return Vec::new();
    };
    let mut errors = Vec::new();

    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == tenancy.source)
    else {
        errors.push(TenancyDeclarationError::new(
            "tenancy.source",
            format!(
                "source `{}` is not declared in databases.yaml",
                tenancy.source
            ),
        ));
        return errors;
    };

    // The predicate is backend-neutral, but the registry gate and the grant
    // CTE are compiled as Postgres statements and proved by the Postgres
    // conformance suite. Declaring tenancy on a backend nothing proves would
    // be a promise this build cannot keep.
    if source.kind != SourceKind::Postgres {
        errors.push(TenancyDeclarationError::new(
            "tenancy.source",
            format!(
                "source `{}` is {:?}; tenancy is supported on Postgres sources only",
                tenancy.source, source.kind
            ),
        ));
    }

    if !is_session_variable(&tenancy.variable) {
        errors.push(TenancyDeclarationError::new(
            "tenancy.variable",
            format!(
                "'{}' is not a session variable name (expected an x-donat-* or x-hasura-* name)",
                tenancy.variable
            ),
        ));
    }
    if tenancy.key.trim().is_empty() {
        errors.push(TenancyDeclarationError::new(
            "tenancy.key",
            "the tenant key column may not be empty".to_string(),
        ));
    }
    if tenancy.registry.status.serving.is_empty() {
        errors.push(TenancyDeclarationError::new(
            "tenancy.registry.status.serving",
            "no status is served, so every request would be refused".to_string(),
        ));
    }

    let tracked: BTreeMap<String, &crate::types::TableEntry> = source
        .tables
        .iter()
        .map(|entry| (entry.table.to_string(), entry))
        .collect();

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (index, entry) in tenancy.keys.iter().enumerate() {
        let path = format!("tenancy.keys[{index}]");
        require_tracked(&entry.table, source, &tracked, &path, &mut errors);
        if entry.key.trim().is_empty() {
            errors.push(TenancyDeclarationError::new(
                format!("{path}.key"),
                "the tenant key column may not be empty".to_string(),
            ));
        }
        if !claimed.insert(entry.table.to_string()) {
            errors.push(TenancyDeclarationError::new(
                path,
                format!("table `{}` is declared twice", entry.table),
            ));
        }
    }

    for (index, entry) in tenancy.exempt.iter().enumerate() {
        let path = format!("tenancy.exempt[{index}]");
        let table_entry = require_tracked(&entry.table, source, &tracked, &path, &mut errors);
        match (&entry.shared, &entry.scope_via) {
            (Some(_), Some(_)) => errors.push(TenancyDeclarationError::new(
                &path,
                "declares both `shared` and `scope_via`; a table is either platform \
                 reference data or scoped through a relationship, not both"
                    .to_string(),
            )),
            (None, None) => errors.push(TenancyDeclarationError::new(
                &path,
                format!(
                    "table `{}` is exempt but says nothing about why: add `shared: read_only` \
                     for platform reference data, or `scope_via: <array relationship>` for a row \
                     that belongs to several tenants. An exemption with neither is a table every \
                     tenant can write, which is a side channel between them.",
                    entry.table
                ),
            )),
            _ => {}
        }
        if let (Some(relationship), Some(table_entry)) = (&entry.scope_via, table_entry)
            && !table_entry
                .array_relationships
                .iter()
                .any(|rel| &rel.name == relationship)
        {
            errors.push(TenancyDeclarationError::new(
                format!("{path}.scope_via"),
                format!(
                    "table `{}` declares no array relationship named `{relationship}`",
                    entry.table
                ),
            ));
        }
        if entry.shared.is_some()
            && let Some(table_entry) = table_entry
        {
            report_shared_writers(entry, table_entry, &path, &mut errors);
        }
        if entry.scope_via.is_some()
            && let Some(table_entry) = table_entry
        {
            report_scope_via_writers(entry, table_entry, &path, &mut errors);
        }
        if !claimed.insert(entry.table.to_string()) {
            errors.push(TenancyDeclarationError::new(
                path,
                format!(
                    "table `{}` is declared twice (a table is keyed or exempt, never both)",
                    entry.table
                ),
            ));
        }
    }

    // The registry is the sharpest of these: exempting it would publish every
    // tenant to every tenant.
    if tenancy.is_shared(&tenancy.registry.table) {
        errors.push(TenancyDeclarationError::new(
            "tenancy.registry.table",
            format!(
                "the registry `{}` is exempt, which would let a member of any tenant read every \
                 tenant. Declare it under `keys:` with its own identifier column instead.",
                tenancy.registry.table
            ),
        ));
    }
    require_tracked(
        &tenancy.registry.table,
        source,
        &tracked,
        "tenancy.registry.table",
        &mut errors,
    );

    for (index, entry) in tenancy.cross_tenant_reads.iter().enumerate() {
        let path = format!("tenancy.cross_tenant_reads[{index}]");
        let table_entry = require_tracked(&entry.table, source, &tracked, &path, &mut errors);
        if !is_session_variable(&entry.scoped_by.variable) {
            errors.push(TenancyDeclarationError::new(
                format!("{path}.scoped_by.variable"),
                format!(
                    "'{}' is not a session variable name",
                    entry.scoped_by.variable
                ),
            ));
        }
        // Replacing a predicate that was never going to be applied is dead
        // configuration, and dead configuration around an isolation rule reads
        // like a guarantee that is not there.
        if !matches!(tenancy.table_scope(&entry.table), TableScope::Key(_)) {
            errors.push(TenancyDeclarationError::new(
                &path,
                format!(
                    "table `{}` is exempt, so it carries no tenant predicate for this entry to \
                     replace",
                    entry.table
                ),
            ));
        }
        if let Some(table_entry) = table_entry {
            if !table_entry
                .select_permissions
                .iter()
                .any(|permission| permission.role == entry.role)
                && !table_entry
                    .command_select_permissions
                    .iter()
                    .any(|permission| permission.role == entry.role)
            {
                errors.push(TenancyDeclarationError::new(
                    &path,
                    format!(
                        "role `{}` holds no select permission on `{}`",
                        entry.role, entry.table
                    ),
                ));
            }
            // The role can see rows outside its own tenant. Letting the same
            // role write this table through ordinary CRUD would turn a
            // deliberate read into the leak the whole design is about.
            let holds = |kind: &str, held: bool, errors: &mut Vec<TenancyDeclarationError>| {
                if held {
                    errors.push(TenancyDeclarationError::new(
                        &path,
                        format!(
                            "role `{}` reads `{}` across tenants and also holds an ordinary \
                             {kind} permission on it",
                            entry.role, entry.table
                        ),
                    ));
                }
            };
            holds(
                "insert",
                table_entry
                    .insert_permissions
                    .iter()
                    .any(|permission| permission.role == entry.role),
                &mut errors,
            );
            holds(
                "update",
                table_entry
                    .update_permissions
                    .iter()
                    .any(|permission| permission.role == entry.role),
                &mut errors,
            );
            holds(
                "delete",
                table_entry
                    .delete_permissions
                    .iter()
                    .any(|permission| permission.role == entry.role),
                &mut errors,
            );
        }
    }

    validate_command_tenancy(metadata, tenancy, &mut errors);

    errors
}

fn require_tracked<'a>(
    table: &QualifiedTable,
    source: &'a Source,
    tracked: &BTreeMap<String, &'a crate::types::TableEntry>,
    path: &str,
    errors: &mut Vec<TenancyDeclarationError>,
) -> Option<&'a crate::types::TableEntry> {
    match tracked.get(&table.to_string()) {
        Some(entry) => Some(entry),
        None => {
            errors.push(TenancyDeclarationError::new(
                path,
                format!("table `{table}` is not tracked in source `{}`", source.name),
            ));
            None
        }
    }
}

/// A `shared: read_only` table with a write permission is the exemption
/// undone: the rows are reachable from every tenant, so one of them writing
/// what all of them read is a channel between tenants.
fn report_shared_writers(
    entry: &TenantExemption,
    table_entry: &crate::types::TableEntry,
    path: &str,
    errors: &mut Vec<TenancyDeclarationError>,
) {
    let mut writers: Vec<(&str, &str)> = Vec::new();
    for permission in &table_entry.insert_permissions {
        writers.push(("insert", &permission.role));
    }
    for permission in &table_entry.update_permissions {
        writers.push(("update", &permission.role));
    }
    for permission in &table_entry.delete_permissions {
        writers.push(("delete", &permission.role));
    }
    for permission in &table_entry.command_insert_permissions {
        writers.push(("command insert", &permission.role));
    }
    for permission in &table_entry.command_update_permissions {
        writers.push(("command update", &permission.role));
    }
    for permission in &table_entry.command_delete_permissions {
        writers.push(("command delete", &permission.role));
    }
    for (kind, role) in writers {
        errors.push(TenancyDeclarationError::new(
            path,
            format!(
                "table `{}` is shared read-only, but role `{role}` holds a {kind} permission on \
                 it. A shared writable table is a side channel between tenants.",
                entry.table
            ),
        ));
    }
}

/// A `scope_via` row belongs to several tenants at once, so there is no single
/// value a write could be bounded by — the tenant predicate a writer would get
/// is the one predicate this design cannot produce.
///
/// Ordinary CRUD permissions on such a table are therefore refused outright.
/// Command permissions are not: a command declares where its rows come from
/// and is reviewed as a whole, which is exactly how a person is added to their
/// first tenant before they belong to any.
fn report_scope_via_writers(
    entry: &TenantExemption,
    table_entry: &crate::types::TableEntry,
    path: &str,
    errors: &mut Vec<TenancyDeclarationError>,
) {
    let mut writers: Vec<(&str, &str)> = Vec::new();
    for permission in &table_entry.insert_permissions {
        writers.push(("insert", &permission.role));
    }
    for permission in &table_entry.update_permissions {
        writers.push(("update", &permission.role));
    }
    for permission in &table_entry.delete_permissions {
        writers.push(("delete", &permission.role));
    }
    for (kind, role) in writers {
        errors.push(TenancyDeclarationError::new(
            path,
            format!(
                "table `{}` is scoped through a relationship, but role `{role}` holds an \
                 ordinary {kind} permission on it. A row belonging to several tenants has no \
                 single tenant a write can be bounded by; write it from a command instead.",
                entry.table
            ),
        ));
    }
}

/// The two declarations that let a command step outside the caller's tenant,
/// and the rules that keep each of them countable on one hand.
fn validate_command_tenancy(
    metadata: &Metadata,
    tenancy: &TenancyMetadata,
    errors: &mut Vec<TenancyDeclarationError>,
) {
    for (index, command) in metadata.commands.iter().enumerate() {
        if command.source != tenancy.source {
            continue;
        }
        let path = format!("commands[{index}]");

        if let Some(declared) = &command.tenant {
            let reference = declared.step();
            let Some(step) = command
                .steps
                .iter()
                .find(|step| step.name == reference.step)
            else {
                errors.push(TenancyDeclarationError::new(
                    format!("{path}.tenant"),
                    format!(
                        "command `{}` takes its tenant from step `{}`, which it does not declare",
                        command.name, reference.step
                    ),
                ));
                continue;
            };
            // `establishes` must point at the step that creates the tenant, and
            // `from` at the one that read it. Getting these the wrong way round
            // would produce a statement that compiles and scopes nothing.
            let shape_is_right = match declared {
                crate::types::CommandTenant::Establishes { .. } => matches!(
                    step.operation,
                    crate::types::CommandStepOperation::Insert { .. }
                ),
                crate::types::CommandTenant::From { .. } => matches!(
                    step.operation,
                    crate::types::CommandStepOperation::SelectOne { .. }
                ),
            };
            if !shape_is_right {
                let (what, wanted) = if declared.establishes() {
                    ("establishes", "insert")
                } else {
                    ("from", "select_one")
                };
                errors.push(TenancyDeclarationError::new(
                    format!("{path}.tenant"),
                    format!(
                        "command `{}` declares `tenant: {what}` against step `{}`, which is not \
                         an {wanted} step",
                        command.name, reference.step
                    ),
                ));
            }
        }

        // A command that takes its tenant from a step has no session tenant to
        // bound a write by, and the value it does have lives in another CTE —
        // which a row predicate cannot reference. An insert is safe anyway,
        // because the preset pins the column it writes. An update or a delete
        // is not: it would be bounded only by its own `where`, and an update
        // would additionally move whatever it matched into this command's
        // tenant. There is no predicate that would fix it, so the shape is
        // refused instead.
        if command.tenant.is_some() {
            for (step_index, step) in command.steps.iter().enumerate() {
                let kind = match &step.operation {
                    crate::types::CommandStepOperation::Update { .. } => "update",
                    crate::types::CommandStepOperation::UpdateMany { .. } => "update_many",
                    crate::types::CommandStepOperation::UpdateWhen { .. } => "update_when",
                    crate::types::CommandStepOperation::Delete { .. } => "delete",
                    _ => continue,
                };
                errors.push(TenancyDeclarationError::new(
                    format!("{path}.steps[{step_index}]"),
                    format!(
                        "command `{}` takes its tenant from a step, so step `{}` — an {kind} — \
                         cannot be bounded by one: the tenant it would compare against lives in \
                         another CTE, which a row predicate cannot reach. Split the write into a \
                         command scoped by the session, or express it as an insert.",
                        command.name, step.name
                    ),
                ));
            }
        }

        for (step_index, step) in command.steps.iter().enumerate() {
            if step.tenant != Some(crate::types::StepTenant::Unscoped) {
                continue;
            }
            let step_path = format!("{path}.steps[{step_index}]");
            if tenancy.unscoped_steps != UnscopedStepPolicy::Audited {
                errors.push(TenancyDeclarationError::new(
                    &step_path,
                    format!(
                        "step `{}` reads outside the tenant, but this deployment has not opened \
                         that escape. Set `unscoped_steps: audited` to allow it, and expect every \
                         use to be reviewed.",
                        step.name
                    ),
                ));
            }
            if !matches!(
                step.operation,
                crate::types::CommandStepOperation::SelectOne { .. }
            ) {
                errors.push(TenancyDeclarationError::new(
                    &step_path,
                    format!(
                        "step `{}` reads outside the tenant, which is legal only on a `select_one` \
                         — the unguessable lookup key is the whole authorization",
                        step.name
                    ),
                ));
            }
            // Without this the command would read one tenant's row and then
            // write with the caller's tenant, which is worse than either.
            let scoped_by_it = command
                .tenant
                .as_ref()
                .is_some_and(|declared| declared.step().step == step.name);
            if !scoped_by_it {
                errors.push(TenancyDeclarationError::new(
                    &step_path,
                    format!(
                        "step `{}` reads outside the tenant, but command `{}` does not declare \
                         `tenant: {{ from: {} }}`, so nothing scopes the rest of the statement by \
                         what that row said",
                        step.name, command.name, step.name
                    ),
                ));
            }
        }
    }
}

/// Declarations that only mean something in a tenanted source.
///
/// A `tenant:` block or an `unscoped` step in a deployment with no tenancy is
/// not harmless — it reads as a guarantee that nothing is enforcing.
pub fn validate_untenanted_commands(metadata: &Metadata) -> Vec<TenancyDeclarationError> {
    let mut errors = Vec::new();
    for (index, command) in metadata.commands.iter().enumerate() {
        let tenanted = metadata
            .tenancy
            .as_ref()
            .is_some_and(|tenancy| tenancy.source == command.source);
        if tenanted {
            continue;
        }
        let path = format!("commands[{index}]");
        if command.tenant.is_some() {
            errors.push(TenancyDeclarationError::new(
                format!("{path}.tenant"),
                format!(
                    "command `{}` declares where its tenant comes from, but source `{}` is not \
                     tenanted",
                    command.name, command.source
                ),
            ));
        }
        for (step_index, step) in command.steps.iter().enumerate() {
            if step.tenant.is_some() {
                errors.push(TenancyDeclarationError::new(
                    format!("{path}.steps[{step_index}].tenant"),
                    format!(
                        "step `{}` declares a tenant scope, but source `{}` is not tenanted",
                        step.name, command.source
                    ),
                ));
            }
        }
    }
    errors
}
