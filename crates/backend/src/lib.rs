//! Backend-neutral primitives for donat's multi-backend abstraction.
//!
//! This is a pure-logic, DB-free leaf crate. It holds the building blocks
//! shared across SQL backends: a logical scalar type system (de-leaking the
//! stringly-typed Postgres `pg_type`), a per-backend capability descriptor,
//! and a dialect-rendering trait with a Postgres implementation whose output
//! is byte-identical to the engine's current Postgres rendering.

pub mod capabilities;
pub mod dialect;
pub mod scalar;

pub use capabilities::Capabilities;
pub use dialect::{AnyDialect, Dialect, PostgresDialect, SqliteDialect};
pub use scalar::ScalarType;
