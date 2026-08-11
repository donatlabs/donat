#![forbid(unsafe_code)]

//! The connector SDK.
//!
//! Every connector in this workspace is hand-written Rust against a provider's
//! own published documentation (see
//! `knowledgebase/declarative-saas/decisions/037-*`). This crate owns the half
//! that would otherwise be rewritten for each of them: bounded fixed-origin
//! transport, credential application, pagination, error mapping, and webhook
//! verification.
//!
//! There is deliberately no API here that accepts a caller-supplied URL,
//! method, header name, redirect policy, or TLS policy.
//!
//! A connector is a [`sdk::Connector`]: one static declaration of a provider's
//! name, contract version, origin, credential, operations with their effect
//! classes, and triggers. Spec 010 §11 puts the one
//! `&'static [&'static Connector]` table in this crate; until the two existing
//! provider modules can move here — they need `donat-connector-abi` and
//! `donat-ir` as dependencies first — the table lives beside them in
//! `donat_server::connectors`, and everything that reads a connector reads it
//! through that one table.
//!
//! [`local`] is the other half of the same idea, for work that has no provider
//! at all (spec 018): a capability whose executor is compiled into this binary,
//! declared with its own bounds and admitted only as `Effect::Pure`, on
//! determinism proven by a double render at registration. It is a separate
//! declaration rather than a `Connector` with an empty origin — see
//! `knowledgebase/declarative-saas/decisions/044-*`.

pub mod local;
pub mod providers;
pub mod sdk;
