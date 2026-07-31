//! Pure deploy-time compilation for source-local durable Process definitions.
//!
//! Runtime journals, workers, leases, and connector execution deliberately do
//! not live here.  This module turns finite metadata plus immutable dependency
//! descriptors into one immutable, fingerprinted catalog.

mod activity;
mod catalog;
mod command;
mod definition;
mod inbound;
mod reconcile;
mod runtime;
mod signal;
mod start;
mod timer;
mod transition;
mod value;

pub use activity::*;
pub use catalog::*;
pub use command::*;
pub use definition::*;
pub use inbound::*;
pub use reconcile::*;
pub use runtime::*;
pub use signal::*;
pub use start::*;
pub use transition::*;
