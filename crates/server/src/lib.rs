//! Library facade for the server crate so integration tests in `tests/`
//! can drive the real runtime (`AppState` + `gql::execute_full`) without
//! going through the HTTP layer. The binary entry point lives in
//! `main.rs`; this exposes the same modules as a library target.

pub mod action;
pub mod cron;
pub mod events;
pub mod gql;
pub mod jwt;
pub mod migrate;
pub mod remote;
pub mod state;
pub mod ws;
