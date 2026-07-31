//! Server-neutral contracts used by the two-stage Process/Command compiler.
//!
//! This module deliberately contains no runtime handles, connector executors,
//! database clients, or journal access. The server-owned Process compiler
//! constructs these values and the schema-owned Command compiler consumes
//! them to pin raw effects to immutable Process revisions.

use std::collections::{BTreeMap, BTreeSet};

use donat_ir::{ProcessStartPolicy, TypeRef, ValueContractCatalog};
use donat_metadata::{CommandIdempotencyKey, CommandValue};

use crate::commands::CompiledCommand;

/// Exact source-qualified Process contracts visible to Command finalization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessEffectContractCatalog {
    pub sources: BTreeMap<String, BTreeMap<String, ProcessEffectContract>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEffectContract {
    pub process_name: String,
    pub current_revision: String,
    pub start_policy: ProcessStartPolicy,
    pub start_input: ValueContractCatalog,
    pub process_key: Option<TypeRef>,
    pub signals: BTreeMap<String, ProcessSignalEffectContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSignalEffectContract {
    pub signal_name: String,
    pub contract_revision: String,
    pub correlation: ValueContractCatalog,
    pub payload: ValueContractCatalog,
    pub compatible_revisions: BTreeSet<String>,
}

/// The only dependency Command finalization has on Process compilation.
///
/// Keeping this trait object-safe lets `donat-schema` remain below
/// `donat-server` in the crate graph.
pub trait ProcessEffectContractSource {
    fn process_effect_contract(
        &self,
        source: &str,
        process: &str,
    ) -> Option<&ProcessEffectContract>;
}

impl ProcessEffectContractSource for ProcessEffectContractCatalog {
    fn process_effect_contract(
        &self,
        source: &str,
        process: &str,
    ) -> Option<&ProcessEffectContract> {
        self.sources.get(source)?.get(process)
    }
}

/// A Command effect after its Process target and compatible revision have
/// been pinned. Values remain symbolic until request planning resolves them
/// against arguments, prior rows, and the explicit session.
#[derive(Debug, Clone)]
pub enum FinalizedCommandEffect {
    Start(FinalizedStartProcessEffect),
    Signal(FinalizedSignalProcessEffect),
}

#[derive(Debug, Clone)]
pub struct FinalizedStartProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub start_policy: ProcessStartPolicy,
    pub process_key: Option<CommandValue>,
    pub input: BTreeMap<String, CommandValue>,
    pub semantic_idempotency_key: CommandIdempotencyKey,
    pub effect_position: u32,
}

#[derive(Debug, Clone)]
pub struct FinalizedSignalProcessEffect {
    pub source: String,
    pub process_name: String,
    pub process_revision: String,
    pub signal_name: String,
    pub correlation: BTreeMap<String, CommandValue>,
    pub payload: BTreeMap<String, CommandValue>,
    pub semantic_idempotency_key: CommandIdempotencyKey,
    pub compatible_revisions: BTreeSet<String>,
    pub effect_position: u32,
}

#[derive(Debug, Clone, Default)]
pub struct FinalizedCommandCatalog {
    pub sources: BTreeMap<String, FinalizedSourceCommandCatalog>,
}

impl FinalizedCommandCatalog {
    pub fn source(&self, source: &str) -> Option<&FinalizedSourceCommandCatalog> {
        self.sources.get(source)
    }
}

#[derive(Debug, Clone)]
pub struct FinalizedSourceCommandCatalog {
    pub source_name: String,
    pub commands: BTreeMap<String, FinalizedCompiledCommand>,
}

impl FinalizedSourceCommandCatalog {
    pub fn command(&self, name: &str) -> Option<&FinalizedCompiledCommand> {
        self.commands.get(name)
    }
}

#[derive(Debug, Clone)]
pub struct FinalizedCompiledCommand {
    pub command: CompiledCommand,
    pub effects: Vec<FinalizedCommandEffect>,
}
