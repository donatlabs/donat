//! The server crate's one module tree.
//!
//! Integration tests in `tests/` drive the real runtime (`AppState` +
//! `gql::execute_full`) through this facade without going through the HTTP
//! layer, and `main.rs` builds its router from the same modules rather than
//! declaring a second copy of them. Compiling the tree twice used to make
//! every item that only the library facade reaches look dead from the
//! binary's side, which buried real dead code in a dozen false positives.

pub mod action;
pub mod codegen;
pub mod commands;
pub mod connector_webhook;
pub mod connectors;
pub mod cron;
pub mod events;
pub mod files;
pub mod gql;
pub mod jwt;
pub mod mcp;
pub mod migrate;
pub mod pgtls;
pub mod processes;
pub mod remote;
pub mod rest;
pub mod shutdown;
pub mod state;
pub mod upstream;
pub mod validate;
pub mod ws;
