//! Plan entitlements, compiled into the writes they gate.
//!
//! This exists because of a constraint the layering creates on purpose. A
//! business domain exposes its tables as ordinary CRUD with its own insert
//! permissions. A platform on top of it has to cap how many rows a tenant may
//! hold — and it may not edit that permission to add the check, because an
//! overlay that can quietly rewrite the base would make every audit of the
//! base meaningless.
//!
//! The answer is the one tenancy already uses: add a layer, do not edit the
//! text. `tenancy.yaml` ANDs a tenant predicate into every permission without
//! touching a line of it; a quota ANDs a ceiling into the check the same way.
//!
//! **A quota is never a `COUNT(*)` taken beforehand.** Under READ COMMITTED a
//! statement's snapshot is fixed before it starts executing, so every
//! concurrent writer counts the same pre-lock state and every one of them
//! passes. The counter is moved *inside* the statement that performs the
//! write, which takes a row lock on the tenant's usage row and makes the
//! second writer wait, re-read, and be refused.

use serde::{Deserialize, Serialize};

use crate::tenancy::same_table;
use crate::types::{Metadata, QualifiedTable};

/// Where the per-tenant counters live.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaCounters {
    pub table: QualifiedTable,
    pub tenant: crate::tenancy::ColumnBinding,
}

/// Where the ceilings live, and how a tenant reaches its own.
///
/// Raising a limit is then a row change rather than a deploy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaLimits {
    pub table: QualifiedTable,
    pub key: crate::tenancy::ColumnBinding,
    pub via: QuotaLimitLookup,
}

/// The registry column naming which plan a tenant is on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaLimitLookup {
    pub table: QualifiedTable,
    pub column: String,
    /// The registry column a tenant is found by. Defaults to the tenancy key
    /// of that table, which is what a registry row is identified by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_on: Option<String>,
}

/// One thing a plan caps.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entitlement {
    pub name: String,
    /// The counter column in the usage table.
    pub counter: String,
    /// The ceiling column in the plan table.
    pub maximum: String,
    /// Every insert into these tables consumes one unit; every delete releases
    /// one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumes: Vec<QuotaConsumer>,
}

/// A table whose rows are counted against an entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaConsumer {
    pub table: QualifiedTable,
}

/// Plan entitlements for one source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaMetadata {
    pub source: String,
    pub counters: QuotaCounters,
    pub limits: QuotaLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entitlements: Vec<Entitlement>,
}

impl QuotaMetadata {
    /// The entitlement an insert into this table consumes, if any.
    pub fn consumed_by(&self, table: &QualifiedTable) -> Option<&Entitlement> {
        self.entitlements.iter().find(|entitlement| {
            entitlement
                .consumes
                .iter()
                .any(|consumer| same_table(&consumer.table, table))
        })
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One refusal, naming the metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDeclarationError {
    pub path: String,
    pub message: String,
}

impl QuotaDeclarationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for QuotaDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// Every quota rule decidable from metadata alone.
pub fn validate_quota_declaration(metadata: &Metadata) -> Vec<QuotaDeclarationError> {
    let Some(quotas) = &metadata.quotas else {
        return Vec::new();
    };
    let mut errors = Vec::new();

    // A counter is per tenant; without tenancy there is nothing to count
    // against and the ceiling would be a deployment-wide one nobody asked for.
    match &metadata.tenancy {
        Some(tenancy) if tenancy.source == quotas.source => {}
        _ => errors.push(QuotaDeclarationError::new(
            "quotas.source",
            format!(
                "source `{}` declares quotas but no tenancy; a ceiling is held by a tenant",
                quotas.source
            ),
        )),
    }

    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == quotas.source)
    else {
        errors.push(QuotaDeclarationError::new(
            "quotas.source",
            format!(
                "source `{}` is not declared in databases.yaml",
                quotas.source
            ),
        ));
        return errors;
    };
    let tracked = |table: &QualifiedTable| {
        source
            .tables
            .iter()
            .any(|entry| same_table(&entry.table, table))
    };

    for (label, table) in [
        ("quotas.counters.table", &quotas.counters.table),
        ("quotas.limits.table", &quotas.limits.table),
        ("quotas.limits.via.table", &quotas.limits.via.table),
    ] {
        if !tracked(table) {
            errors.push(QuotaDeclarationError::new(
                label,
                format!("`{table}` is not tracked in source `{}`", quotas.source),
            ));
        }
    }

    // A ceiling a tenant can move is not a ceiling. Counters, plan limits and
    // the registry row naming which plan a tenant is on are all read by the
    // gate, so a tenant-facing write permission on any of them is the gate
    // undone: reset the counter, raise the maximum, or switch to a richer
    // plan. This is the same refusal `tenancy.exempt` makes for shared tables.
    for (label, table) in [
        ("quotas.counters.table", &quotas.counters.table),
        ("quotas.limits.table", &quotas.limits.table),
        ("quotas.limits.via.table", &quotas.limits.via.table),
    ] {
        let Some(entry) = source
            .tables
            .iter()
            .find(|entry| same_table(&entry.table, table))
        else {
            continue;
        };
        for (kind, roles) in [
            (
                "insert",
                entry
                    .insert_permissions
                    .iter()
                    .map(|permission| permission.role.clone())
                    .collect::<Vec<_>>(),
            ),
            (
                "update",
                entry
                    .update_permissions
                    .iter()
                    .map(|permission| permission.role.clone())
                    .collect::<Vec<_>>(),
            ),
            (
                "delete",
                entry
                    .delete_permissions
                    .iter()
                    .map(|permission| permission.role.clone())
                    .collect::<Vec<_>>(),
            ),
        ] {
            for role in roles {
                errors.push(QuotaDeclarationError::new(
                    label,
                    format!(
                        "role `{role}` holds an ordinary {kind} permission on `{table}`, which \
                         the ceiling is computed from — a tenant that can write it can lift its \
                         own limit. Move the write into a platform command."
                    ),
                ));
            }
        }
    }

    let mut names = std::collections::BTreeSet::new();
    for (index, entitlement) in quotas.entitlements.iter().enumerate() {
        let path = format!("quotas.entitlements[{index}]");
        if !names.insert(entitlement.name.as_str()) {
            errors.push(QuotaDeclarationError::new(
                &path,
                format!("entitlement `{}` is declared twice", entitlement.name),
            ));
        }
        // A ceiling with no writer to gate is fiction, and fiction in a limits
        // table is how a plan quietly stops meaning anything.
        if entitlement.consumes.is_empty() {
            errors.push(QuotaDeclarationError::new(
                &path,
                format!(
                    "entitlement `{}` caps `{}` but nothing consumes it, so the ceiling would \
                     never be reached however many rows a tenant creates",
                    entitlement.name, entitlement.maximum
                ),
            ));
        }
        for consumer in &entitlement.consumes {
            let Some(entry) = source
                .tables
                .iter()
                .find(|entry| same_table(&entry.table, &consumer.table))
            else {
                errors.push(QuotaDeclarationError::new(
                    &path,
                    format!(
                        "`{}` is not tracked in source `{}`",
                        consumer.table, quotas.source
                    ),
                ));
                continue;
            };
            // The counter moves inside the statement an *ordinary* insert or
            // delete performs. A command's steps carry no counter, so a row
            // written through one would not be counted — and a ceiling that
            // one write path ignores is a ceiling a tenant walks around by
            // choosing that path. Refused here rather than left as a gap:
            // counting a command's writes is a feature this does not have yet,
            // and a limit that is true of some writes is worse than one that
            // is true of none.
            let command_writers = entry
                .command_insert_permissions
                .iter()
                .map(|permission| ("command insert", permission.role.clone()))
                .chain(
                    entry
                        .command_delete_permissions
                        .iter()
                        .map(|permission| ("command delete", permission.role.clone())),
                )
                .collect::<Vec<_>>();
            for (kind, role) in command_writers {
                errors.push(QuotaDeclarationError::new(
                    &path,
                    format!(
                        "entitlement `{}` counts rows of `{}`, but role `{role}` holds a {kind} \
                         permission on it — a command's steps carry no counter, so that write \
                         would cross the ceiling without moving it.",
                        entitlement.name, consumer.table
                    ),
                ));
            }
        }
    }

    errors
}
