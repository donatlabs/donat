//! The tenancy migration, derived instead of written.
//!
//! `tenancy.yaml` is sixteen meaningful lines and the migration under it is
//! four hundred and ninety-eight — a column and an index on every tracked
//! table, views carrying the driving table's key, natural keys rescoped,
//! references made composite. All of it follows from the declaration plus the
//! catalogue, which is why it is derived here rather than remembered.
//!
//! The reason this is worth a module and not a checklist is the fourth
//! derivation. Once `customer.customer_id` is unique only *within* a store,
//! two stores hold that id legitimately, and a unique index over it anywhere
//! else quietly becomes a cross-store constraint. Petshop had exactly one, and
//! it meant a shopper with an open cart in one store could not open one in
//! another. A person reads the migration and does not see it; this reads the
//! catalogue and cannot miss it.
//!
//! What it will not do is guess. Three shapes are refused by name — a view
//! whose body it cannot re-derive, a table that already holds rows, and a
//! unique key it cannot classify as chosen or issued — because a half-scoped
//! migration reads exactly like a finished one.

use std::collections::{BTreeMap, BTreeSet};

use donat_catalog_types::Catalog;
use donat_metadata::{Metadata, QualifiedTable, TableScope};

/// A unique index or constraint as the database holds it.
///
/// Gathered separately from the shared catalogue, which keeps only
/// unconditional ones because the planner has no use for the rest — and the
/// constraint that prompted this module was partial. Carrying the name as well
/// is what makes a rescope expressible: `unique_keys` is column sets alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueIndex {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub columns: Vec<String>,
    /// `WHERE …` for a partial index; `None` for a total one.
    pub predicate: Option<String>,
    /// Backed by a constraint, so it is dropped with `DROP CONSTRAINT` rather
    /// than `DROP INDEX`.
    pub constraint: bool,
}

/// One statement the declaration implies and the database does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenancyChange {
    AddColumn {
        table: String,
        column: String,
        sql_type: String,
        /// The tenant every row already there belongs to. `None` where the
        /// table is empty and the column can be `NOT NULL` from the start.
        backfill: Option<String>,
    },
    AddIndex {
        table: String,
        column: String,
    },
    /// A unique key that has to carry the tenant, because something it is
    /// keyed on is only unique within one.
    ScopeUnique {
        index: UniqueIndex,
        tenant: String,
        /// The column whose identity became tenant-scoped, so the message can
        /// say why rather than assert.
        because: String,
    },
    /// A reference into a table whose identity became `(tenant, …)`.
    CompositeForeignKey {
        table: String,
        constraint: String,
        referenced: String,
        mapping: BTreeMap<String, String>,
        tenant: String,
    },
}

/// Something the generator will not decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub object: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenancyPlan {
    pub changes: Vec<TenancyChange>,
    pub unresolved: Vec<Unresolved>,
}

impl TenancyPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.unresolved.is_empty()
    }
}

fn qualified(table: &QualifiedTable) -> String {
    format!("{}.{}", table.schema(), table.name())
}

/// Derive what the declaration implies and the database does not have.
///
/// `uniques` is every unique index and constraint in the source, including the
/// partial ones the shared catalogue drops.
pub fn plan_tenancy(
    metadata: &Metadata,
    catalog: &Catalog,
    uniques: &[UniqueIndex],
    populated: &BTreeSet<String>,
    backfill: Option<&str>,
) -> TenancyPlan {
    let mut plan = TenancyPlan::default();
    let Some(tenancy) = &metadata.tenancy else {
        return plan;
    };
    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == tenancy.source)
    else {
        return plan;
    };

    // The tenant column's type is the registry's own key, so the two compare.
    let tenant_type = catalog
        .table(
            tenancy.registry.table.schema(),
            tenancy.registry.table.name(),
        )
        .and_then(|info| {
            info.columns
                .iter()
                .find(|column| column.name == tenancy.registry.key)
        })
        .map(|column| column.sql_type().to_string())
        .unwrap_or_else(|| "text".to_string());

    // Which tables carry which key, so the second-order rule below can ask
    // whether a referenced identity is tenant-scoped.
    let mut key_of: BTreeMap<String, String> = BTreeMap::new();
    for entry in &source.tables {
        if let TableScope::Key(key) = tenancy.table_scope(&entry.table) {
            key_of.insert(qualified(&entry.table), key.to_string());
        }
    }

    for entry in &source.tables {
        let name = qualified(&entry.table);
        let Some(key) = key_of.get(&name).cloned() else {
            continue; // shared or scoped through a relationship: nothing to add
        };
        let Some(info) = catalog.table(entry.table.schema(), entry.table.name()) else {
            continue; // absent from the database is `validate`'s finding, not ours
        };

        // A view carries its driving table's key, and re-deriving the body
        // needs the SQL it was written with. Named rather than guessed.
        if info.relation_kind != donat_catalog_types::RelationKind::Table {
            if !info.columns.iter().any(|column| column.name == key) {
                plan.unresolved.push(Unresolved {
                    object: name.clone(),
                    reason: format!(
                        "is a view and does not expose \"{key}\". Its body has to carry the \
                         driving table's key — add it to the select list, and to GROUP BY where \
                         the view groups — then re-run."
                    ),
                });
            }
            continue;
        }

        if !info.columns.iter().any(|column| column.name == key) {
            // A column added `NOT NULL` to a table that already holds rows
            // needs a value for them, and only the owner knows which tenant
            // those rows belong to. Turning a single-tenant database into a
            // tenanted one is the ordinary case, and `--backfill` is how the
            // answer is given once.
            let rows = populated.contains(&name);
            match (rows, backfill) {
                (true, None) => {
                    plan.unresolved.push(Unresolved {
                        object: name.clone(),
                        reason: format!(
                            "already holds rows, so \"{key}\" cannot be added NOT NULL without \
                             saying which tenant they belong to. Re-run with `--backfill \
                             <tenant>`; every row already there is given that one."
                        ),
                    });
                    continue;
                }
                _ => plan.changes.push(TenancyChange::AddColumn {
                    table: name.clone(),
                    column: key.clone(),
                    sql_type: tenant_type.clone(),
                    backfill: rows.then(|| backfill.unwrap_or_default().to_string()),
                }),
            }
            plan.changes.push(TenancyChange::AddIndex {
                table: name.clone(),
                column: key.clone(),
            });
        }

        // A reference has to carry the tenant only when what it points at is
        // *itself* unique per tenant. Most references are to a surrogate
        // primary key, which is unique across stores by construction, and
        // widening those buys nothing while touching every table in the
        // schema. The ones that matter point at an identity somebody chose —
        // a customer id, a slug — which two stores hold legitimately.
        for foreign in &info.foreign_keys {
            let referenced = format!("{}.{}", foreign.referenced_schema, foreign.referenced_table);
            let Some(remote_key) = key_of.get(&referenced) else {
                continue;
            };
            if foreign.column_mapping.contains_key(&key) {
                continue; // already composite
            }
            let target: BTreeSet<&str> = foreign
                .column_mapping
                .values()
                .map(String::as_str)
                .collect();
            if !only_unique_with_tenant(uniques, &referenced, remote_key, &target) {
                continue;
            }
            plan.changes.push(TenancyChange::CompositeForeignKey {
                table: name.clone(),
                constraint: foreign.constraint_name.clone(),
                referenced: referenced.clone(),
                mapping: foreign.column_mapping.clone(),
                tenant: key.clone(),
            });
        }
    }

    plan_unique_keys(&key_of, uniques, &mut plan, catalog);
    plan
}

/// Is `target` on `table` unique only when the tenant is added to it?
///
/// True when the database holds a unique key over exactly the tenant plus
/// those columns, and no unique key over those columns alone. That is the
/// signature of an identity somebody chose and a store owns: `customer(tenant,
/// customer_id)` after the natural key was rescoped. A surrogate primary key
/// fails it, which is why references to one are left as they are.
fn only_unique_with_tenant(
    uniques: &[UniqueIndex],
    table: &str,
    tenant: &str,
    target: &BTreeSet<&str>,
) -> bool {
    let on_table = uniques
        .iter()
        .filter(|index| format!("{}.{}", index.schema, index.table) == table);
    let mut with_tenant = false;
    for index in on_table {
        let columns: BTreeSet<&str> = index.columns.iter().map(String::as_str).collect();
        if &columns == target {
            return false; // unique on its own, so the reference is already sound
        }
        let mut wanted = target.clone();
        wanted.insert(tenant);
        if columns == wanted {
            with_tenant = true;
        }
    }
    with_tenant
}

/// Foreign keys elsewhere that rest on exactly these columns of this table.
fn referencing(catalog: &Catalog, schema: &str, table: &str, columns: &[String]) -> Vec<String> {
    let target: BTreeSet<&str> = columns.iter().map(String::as_str).collect();
    let mut found = Vec::new();
    for info in catalog.tables.values() {
        for foreign in &info.foreign_keys {
            if foreign.referenced_schema != schema || foreign.referenced_table != table {
                continue;
            }
            let referenced: BTreeSet<&str> = foreign
                .column_mapping
                .values()
                .map(String::as_str)
                .collect();
            if referenced == target {
                found.push(foreign.constraint_name.clone());
            }
        }
    }
    found.sort();
    found
}

/// The derivation that earns the module.
///
/// A unique key on a tenanted table that does not carry the tenant is one of
/// three things, and only two of them can be decided here.
///
/// It is **fine** when every column it is keyed on is unique across stores by
/// construction — issued by the database or by a provider. It **must be
/// rescoped** when one of them is a reference into an identity that is itself
/// tenant-scoped, because then two stores hold that value legitimately and the
/// index turns a store boundary into a shared constraint. Anything else is a
/// value somebody chose, and whether a slug should be unique per store or
/// across all of them is a question about the business, so it is named and
/// left.
/// The derivation that earns the module.
///
/// A unique key on a tenanted table that does not carry the tenant is one of
/// three things. It is **fine** when any column it is keyed on is unique across
/// stores by construction — a surrogate, or a reference to one — because one
/// such column makes the whole key global. It **must be rescoped** when every
/// column is tenant-bound and one of them reaches an identity that is itself
/// only unique per store: two stores hold that value legitimately and the index
/// turns a store boundary into a shared constraint. Anything else is a value
/// somebody chose, and whether a slug belongs to a store or to the deployment
/// is a question about the business, so it is named and left.
fn plan_unique_keys(
    key_of: &BTreeMap<String, String>,
    uniques: &[UniqueIndex],
    plan: &mut TenancyPlan,
    catalog: &Catalog,
) {
    for index in uniques {
        let table = format!("{}.{}", index.schema, index.table);
        let Some(key) = key_of.get(&table) else {
            continue;
        };
        if index.columns.iter().any(|column| column == key) {
            continue;
        }
        let Some(info) = catalog.table(&index.schema, &index.table) else {
            continue;
        };

        // One globally unique column makes the whole key globally unique, so
        // the rest of it cannot narrow that. This is why most keys need
        // nothing: they are keyed on a surrogate, or on a reference to one.
        let mut tenant_scoped = None;
        let mut globally_scoped = false;
        for column in &index.columns {
            // A default makes it a surrogate; `uuid` makes it one whatever
            // supplies it, because a uuid is a global identifier by what the
            // type is rather than by where it came from. A provider's `text`
            // identifier is deliberately not covered: those are global only
            // while one deployment holds one account.
            let declared = info
                .columns
                .iter()
                .find(|candidate| &candidate.name == column);
            if declared.is_some_and(|candidate| {
                candidate.has_default || candidate.sql_type().eq_ignore_ascii_case("uuid")
            }) {
                globally_scoped = true;
                continue;
            }
            for foreign in &info.foreign_keys {
                if !foreign.column_mapping.contains_key(column) {
                    continue;
                }
                let referenced =
                    format!("{}.{}", foreign.referenced_schema, foreign.referenced_table);
                match key_of.get(&referenced) {
                    Some(remote_key) => {
                        // Without the tenant: the question is whether what
                        // this column points at stands on its own. A composite
                        // reference already carries the tenant, and leaving it
                        // in would answer "sound" about every one of them.
                        let target: BTreeSet<&str> = foreign
                            .column_mapping
                            .values()
                            .map(String::as_str)
                            .filter(|value| value != remote_key)
                            .collect();
                        if only_unique_with_tenant(uniques, &referenced, remote_key, &target) {
                            tenant_scoped = Some(column.clone());
                        } else {
                            globally_scoped = true;
                        }
                    }
                    None => globally_scoped = true,
                }
            }
        }

        if globally_scoped {
            continue;
        }
        if let Some(because) = tenant_scoped {
            // A unique key another table's foreign key rests on cannot be
            // dropped while that key exists, so rewriting it is a sequence
            // across tables rather than one statement. Named rather than
            // emitted, because SQL that fails halfway leaves a schema nobody
            // planned.
            let dependents = referencing(catalog, &index.schema, &index.table, &index.columns);
            if !dependents.is_empty() {
                plan.unresolved.push(Unresolved {
                    object: format!("{table} ({})", index.name),
                    reason: format!(
                        "has to carry \"{key}\" because \"{because}\" is unique only within a \
                         store, but {} rests on it. Dropping it means dropping those first and \
                         restoring them composite, which is a sequence this will not write \
                         blind.",
                        dependents.join(", ")
                    ),
                });
                continue;
            }
            plan.changes.push(TenancyChange::ScopeUnique {
                index: index.clone(),
                tenant: key.clone(),
                because,
            });
            continue;
        }

        plan.unresolved.push(Unresolved {
            object: format!("{table} ({})", index.name),
            reason: format!(
                "is unique on ({}) and carries no tenant, and nothing there is unique across \
                 stores by construction. If those are values somebody chose, the key belongs to \
                 one store and wants \"{key}\" in it; if they are issued by a provider, it is \
                 right as it stands and stays global until credentials are per-tenant. That is a \
                 question about the business, so it is left here.",
                index.columns.join(", ")
            ),
        });
    }
}

/// The plan as SQL, in the order a migration applies it.
pub fn render_sql(plan: &TenancyPlan) -> String {
    let mut out = String::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for change in &plan.changes {
        let statement = match change {
            TenancyChange::AddColumn {
                table,
                column,
                sql_type,
                backfill,
            } => match backfill {
                None => format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type} NOT NULL;"),
                Some(tenant) => format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {sql_type};\n\
                     UPDATE {table} SET {column} = '{tenant}';\n\
                     ALTER TABLE {table} ALTER COLUMN {column} SET NOT NULL;"
                ),
            },
            TenancyChange::AddIndex { table, column } => format!(
                "CREATE INDEX {}_{column}_idx ON {table} ({column});",
                table.replace('.', "_")
            ),
            TenancyChange::ScopeUnique {
                index,
                tenant,
                because,
            } => {
                let table = format!("{}.{}", index.schema, index.table);
                let columns = std::iter::once(tenant.clone())
                    .chain(index.columns.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(", ");
                let drop = if index.constraint {
                    format!("ALTER TABLE {table} DROP CONSTRAINT {};", index.name)
                } else {
                    format!("DROP INDEX {};", index.name)
                };
                let create = match &index.predicate {
                    Some(predicate) => format!(
                        "CREATE UNIQUE INDEX {} ON {table} ({columns}) WHERE {predicate};",
                        index.name
                    ),
                    None => format!("CREATE UNIQUE INDEX {} ON {table} ({columns});", index.name),
                };
                format!(
                    "-- \"{because}\" is unique only within a store, so this is too.\n{drop}\n{create}"
                )
            }
            TenancyChange::CompositeForeignKey {
                table,
                constraint,
                referenced,
                mapping,
                tenant,
            } => {
                let local = std::iter::once(tenant.clone())
                    .chain(mapping.keys().cloned())
                    .collect::<Vec<_>>()
                    .join(", ");
                let remote = std::iter::once(tenant.clone())
                    .chain(mapping.values().cloned())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "ALTER TABLE {table} DROP CONSTRAINT {constraint};\n\
                     ALTER TABLE {table} ADD CONSTRAINT {constraint} \
                     FOREIGN KEY ({local}) REFERENCES {referenced} ({remote});"
                )
            }
        };
        if seen.insert(statement.clone()) {
            out.push_str(&statement);
            out.push_str("\n\n");
        }
    }
    out
}
