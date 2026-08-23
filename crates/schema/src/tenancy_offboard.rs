//! Reading a tenant out, and taking it away.
//!
//! Onboarding needs no DDL — `binding: row_key` was chosen so that a store is
//! rows. Offboarding is the same claim read backwards, and it is the deferral
//! that stops a deployment being *run* rather than merely being incomplete: a
//! platform that cannot delete a customer cannot operate under GDPR, and one
//! that cannot hand a customer their data cannot answer portability either.
//!
//! Export and erase are one walk of the same tables in the same order. The
//! order is the only interesting part: a row cannot be deleted while another
//! references it, so children go first, and which is which is a fact the
//! catalogue holds rather than one a person should be maintaining. Writing it
//! by hand is the mistake this branch has already made twice.

use std::collections::{BTreeMap, BTreeSet};

use donat_catalog_types::Catalog;
use donat_metadata::{Metadata, TableScope};

/// One table's part in the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// `schema.name`.
    pub table: String,
    /// How the tenant is found on it.
    pub reach: Reach,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// The tenant key is a column here.
    Key(String),
    /// Reached through a relationship that is itself scoped, as the
    /// declaration says.
    Via {
        relationship: String,
        remote: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub object: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offboarding {
    /// Children before parents: safe to delete in this order, and the reverse
    /// of a sensible order to read them in.
    pub steps: Vec<Step>,
    pub refusals: Vec<Refusal>,
}

/// The walk one tenant's removal takes.
///
/// Ordered so a reference is always broken before what it points at is
/// removed. A cycle among references cannot be ordered at all, and is refused
/// by name rather than resolved arbitrarily — `ON DELETE` behaviour is the
/// deployment's to decide, and guessing it here would delete more than the
/// caller asked for.
pub fn plan_offboarding(metadata: &Metadata, catalog: &Catalog) -> Offboarding {
    let mut plan = Offboarding::default();
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

    let mut reach: BTreeMap<String, Reach> = BTreeMap::new();
    for entry in &source.tables {
        let name = format!("{}.{}", entry.table.schema(), entry.table.name());
        // A view is tracked and carries the key, so the tenant predicate binds
        // it — but it holds no rows of its own. Reading it out would duplicate
        // what its tables already gave, and deleting from it is not something
        // Postgres will do for anything past a single-table select. Its rows
        // leave when the tables under it do.
        if catalog
            .table(entry.table.schema(), entry.table.name())
            .is_some_and(|info| info.relation_kind != donat_catalog_types::RelationKind::Table)
        {
            continue;
        }
        match tenancy.table_scope(&entry.table) {
            TableScope::Key(key) => {
                reach.insert(name, Reach::Key(key.to_string()));
            }
            TableScope::ScopeVia(relationship) => {
                // The declaration already named the relationship; the remote
                // is where the key actually lives.
                let remote = entry
                    .array_relationships
                    .iter()
                    .find(|candidate| candidate.name == relationship)
                    .and_then(|candidate| candidate.using.manual_configuration.as_ref())
                    .map(|manual| {
                        format!(
                            "{}.{}",
                            manual.remote_table.schema(),
                            manual.remote_table.name()
                        )
                    });
                match remote {
                    Some(remote) => {
                        reach.insert(
                            name,
                            Reach::Via {
                                relationship: relationship.to_string(),
                                remote,
                            },
                        );
                    }
                    None => plan.refusals.push(Refusal {
                        object: name,
                        reason: format!(
                            "is scoped through \"{relationship}\", which is not an array \
                             relationship with a manual configuration, so its rows cannot be \
                             reached from the tenant"
                        ),
                    }),
                }
            }
            TableScope::Shared => {
                // Nothing to do and nothing to refuse: a shared table holds
                // nobody's rows in particular, so it is simply not part of a
                // tenant's data.
            }
        }
    }

    plan.steps = order(&reach, catalog, &mut plan.refusals);
    plan
}

/// Children before parents.
///
/// A depth-first walk over the references, emitting a table only once
/// everything that points at it has been emitted. A cycle stops the ordering
/// and is named.
fn order(
    reach: &BTreeMap<String, Reach>,
    catalog: &Catalog,
    refusals: &mut Vec<Refusal>,
) -> Vec<Step> {
    // Who points at me, among the tables in the walk.
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in reach.keys() {
        dependents.entry(name).or_default();
    }
    for name in reach.keys() {
        let Some((schema, table)) = name.split_once('.') else {
            continue;
        };
        let Some(info) = catalog.table(schema, table) else {
            continue;
        };
        for foreign in &info.foreign_keys {
            let referenced = format!("{}.{}", foreign.referenced_schema, foreign.referenced_table);
            if referenced == *name {
                continue; // a self-reference orders nothing
            }
            if let Some((key, list)) = dependents.get_key_value_mut_compat(&referenced) {
                let _ = key;
                list.push(name);
            }
        }
    }

    let mut emitted: BTreeSet<&str> = BTreeSet::new();
    let mut steps = Vec::new();
    let mut path: Vec<&str> = Vec::new();

    fn visit<'a>(
        name: &'a str,
        dependents: &BTreeMap<&'a str, Vec<&'a str>>,
        reach: &'a BTreeMap<String, Reach>,
        emitted: &mut BTreeSet<&'a str>,
        path: &mut Vec<&'a str>,
        steps: &mut Vec<Step>,
        refusals: &mut Vec<Refusal>,
    ) {
        if emitted.contains(name) {
            return;
        }
        if path.contains(&name) {
            refusals.push(Refusal {
                object: name.to_string(),
                reason: format!(
                    "is part of a cycle of references ({}), which cannot be ordered for deletion. \
                     Break it in the schema, or say what `ON DELETE` should do, and re-run.",
                    path.join(" -> ")
                ),
            });
            return;
        }
        path.push(name);
        for dependent in dependents.get(name).map(Vec::as_slice).unwrap_or_default() {
            visit(dependent, dependents, reach, emitted, path, steps, refusals);
        }
        path.pop();
        if emitted.insert(name)
            && let Some(reach) = reach.get(name)
        {
            steps.push(Step {
                table: name.to_string(),
                reach: reach.clone(),
            });
        }
    }

    let names: Vec<&str> = reach.keys().map(String::as_str).collect();
    for name in names {
        visit(
            name,
            &dependents,
            reach,
            &mut emitted,
            &mut path,
            &mut steps,
            refusals,
        );
    }
    steps
}

/// A tiny helper so the borrow above reads plainly.
trait GetKeyValueMutCompat<'a> {
    fn get_key_value_mut_compat(&mut self, key: &str) -> Option<(&'a str, &mut Vec<&'a str>)>;
}

impl<'a> GetKeyValueMutCompat<'a> for BTreeMap<&'a str, Vec<&'a str>> {
    fn get_key_value_mut_compat(&mut self, key: &str) -> Option<(&'a str, &mut Vec<&'a str>)> {
        let owned = *self.keys().find(|candidate| **candidate == key)?;
        self.get_mut(owned).map(|list| (owned, list))
    }
}
