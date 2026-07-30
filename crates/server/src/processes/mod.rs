//! Pure deploy-time compilation for source-local durable Process definitions.
//!
//! Runtime journals, workers, leases, and connector execution deliberately do
//! not live here.  This module turns finite metadata plus immutable dependency
//! descriptors into one immutable, fingerprinted catalog.

mod definition;

pub use definition::*;
