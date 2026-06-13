//! The planner (milestones M2–M5): GraphQL operation + metadata + catalog
//! -> IR, with permissions applied.
//!
//! Resolves root fields by Donat v2 naming, walks selection sets
//! (fragments, aliases, @include/@skip, variables), restricts everything by
//! the role's permissions (column masks, row filters merged into
//! predicates, permission limits), substitutes session variables, and
//! cross-checks every column/relationship against the Postgres catalog.
//!
//! There is deliberately NO admin role and no permission bypass: every
//! request runs as an explicit role, and a table without a select
//! permission for that role simply does not exist in that role's schema.

mod introspection;
mod naming;
mod plan;
mod plan_mutation;
mod predicate;
mod v1;

pub use introspection::execute_introspection;
pub use naming::table_base_name;
pub use plan::{Plan, PlanError, Planner, Session};
