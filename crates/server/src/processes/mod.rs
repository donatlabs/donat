//! Pure deploy-time compilation for source-local durable Process definitions.
//!
//! Runtime journals, workers, leases, and connector execution deliberately do
//! not live here.  This module turns finite metadata plus immutable dependency
//! descriptors into one immutable, fingerprinted catalog.

mod catalog;
mod definition;
mod reconcile;
mod runtime;
mod start;

pub use catalog::*;
pub use definition::*;
pub use reconcile::*;
pub use runtime::*;
pub use start::*;
