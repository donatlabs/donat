//! What one caller may ask for in a single operation.
//!
//! `knowledgebase/operations/decisions/001-bounded-and-drainable-by-default`
//! bounds a request — a statement timeout under a request deadline, an
//! upstream response read against a ceiling. Every one of those is per
//! request, so a thousand cheap operations from one tenant cost what a
//! thousand from a thousand tenants cost.
//!
//! This is the other half, and it is deliberately small. The engine bounds
//! what a reverse proxy cannot see and nothing else — spec 008 states that
//! position and this keeps it. A node count is here because the engine has
//! already parsed the document a proxy would have to reimplement a parser to
//! read; anything a proxy can read from an address or a header is not here and
//! should not be.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

/// A ceiling with an optional per-role override, the shape Hasura's API limits
/// use — one place to say "everyone" and one to say "except".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ceiling {
    /// Applied to any role without an entry below. Absent means no ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_role: BTreeMap<String, u32>,
}

impl Ceiling {
    /// The ceiling that applies to one role, or `None` where neither says.
    pub fn for_role(&self, role: &str) -> Option<u32> {
        self.per_role.get(role).copied().or(self.global)
    }

    pub fn is_empty(&self) -> bool {
        self.global.is_none() && self.per_role.is_empty()
    }
}

/// The optional `limits.yaml` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsMetadata {
    /// Fields in one operation, counted over the parsed document. Not rows:
    /// this bounds how much a caller may ask for in one go, before any of it
    /// is planned.
    #[serde(default, skip_serializing_if = "Ceiling::is_empty")]
    pub nodes: Ceiling,
    /// Operations per minute, counted per tenant where one is declared.
    ///
    /// Only what a proxy cannot key on: the tenant and the role both come out
    /// of a verified token, and verifying it is this engine's job. An address
    /// or a header is a proxy's to bound and has no spelling here.
    #[serde(default, skip_serializing_if = "Ceiling::is_empty")]
    pub requests_per_minute: Ceiling,
}

impl LimitsMetadata {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.requests_per_minute.is_empty()
    }
}
