//! What a local capability *is*: a static declaration of a name, a contract
//! version, and operations that carry an effect class, five bounds, a unit
//! count, and the executor compiled into this binary.
//!
//! The shape deliberately mirrors [`crate::sdk::Connector`] where the two agree
//! — declare, build once, hold in a `static`, hand out `&'static` — and
//! deliberately does not where they do not. A connector's operation renders an
//! HTTP request against a resolved origin with a credential applied; a local
//! operation renders nothing and reaches nothing. Modelling it as a connector
//! would mean giving it an origin and a credential it does not have, and a
//! declaration the runtime ignores is a defect
//! (`knowledgebase/declarative-saas/decisions/034-*`).
//!
//! The executor is a plain `fn` pointer, not a closure and not a trait object,
//! so a capability stays what a connector is: built from constants, with
//! nothing about it decided at request time.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value as JsonValue;

use crate::local::bounds::{LocalBounds, cpu_deadline_exceeded, drained};
use crate::local::canonical_bytes;
use crate::local::context::LocalContext;
use crate::sdk::connector::OperationRejection;
use crate::sdk::effect::{Effect, EffectClass};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::operation::{OperationError, validate_semver_core};

/// The deployment's "stop what you are doing" signal, as a running capability
/// sees it.
///
/// A blocking thread cannot be cancelled, so a drain is cooperative: the
/// dispatcher wires this to the shutdown token and the implementation observes
/// it at [`LocalInvocation::checkpoint`]. What that buys is the property spec
/// 018 §4 asks for — the work ends where it is, with no partial output, and the
/// activity is left retryable rather than abandoned.
#[derive(Debug, Clone, Default)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every execution holding this signal to stop.
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// One execution in progress: its input, its remaining budget, and the two
/// things an implementation must ask the runtime about.
pub struct LocalInvocation<'a> {
    input: &'a JsonValue,
    context: &'a LocalContext,
    bounds: &'a LocalBounds,
    deadline: Instant,
    stop: &'a StopSignal,
    intermediate_used: Cell<usize>,
    units_charged: Cell<u64>,
}

impl LocalInvocation<'_> {
    /// The operation input, already inside the declared input ceiling.
    pub const fn input(&self) -> &JsonValue {
        self.input
    }

    /// The deployment's resolved capability context: the frozen document
    /// templates, and nothing an execution can add to.
    ///
    /// It is a separate argument from the input on purpose. A template is a
    /// deploy-time decision (spec 019 §2), so it may not travel in a value a
    /// process computes; keeping the two apart means there is no branch in
    /// which a request could supply one.
    pub const fn context(&self) -> &LocalContext {
        self.context
    }

    pub const fn bounds(&self) -> &LocalBounds {
        self.bounds
    }

    /// Charge working memory before allocating it.
    ///
    /// The charge is cumulative for one execution, which is what makes it a
    /// *peak* rather than a per-allocation limit: an implementation that needs
    /// a scratch buffer per page charges each one.
    pub fn reserve(&self, bytes: usize) -> Result<(), ConnectorFailure> {
        let used = self.intermediate_used.get().saturating_add(bytes);
        self.bounds.admits_intermediate_bytes(used)?;
        self.intermediate_used.set(used);
        Ok(())
    }

    /// Count units the declaration could not know before work started — pages
    /// a renderer produces, rows a reader finds.
    pub fn charge_units(&self, units: u64) -> Result<(), ConnectorFailure> {
        let counted = self.units_charged.get().saturating_add(units);
        self.bounds.admits_units(counted)?;
        self.units_charged.set(counted);
        Ok(())
    }

    /// The one call an implementation makes inside a loop: it ends the
    /// execution when the deployment is draining or the deadline has passed.
    pub fn checkpoint(&self) -> Result<(), ConnectorFailure> {
        if self.stop.is_stopped() {
            return Err(drained());
        }
        if Instant::now() > self.deadline {
            return Err(cpu_deadline_exceeded());
        }
        Ok(())
    }

    /// Working memory charged so far.
    pub fn intermediate_used(&self) -> usize {
        self.intermediate_used.get()
    }
}

/// Bytes a capability produced, on their way to the attachment store.
///
/// This type has no `Serialize`, no public constructor from a JSON value, and
/// no accessor that yields a JSON body: bytes cannot become an activity result
/// by accident. The dispatcher writes them through `crates/storage` and puts
/// the stored file's identity in the result instead (spec 018 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArtifact {
    attachment: String,
    claim_role: String,
    claim_session_key: Option<String>,
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
}

impl LocalArtifact {
    /// `attachment` is the `<schema>.<table>.<column>` key of the declared file
    /// column these bytes belong to. A produced file is a file like any other:
    /// it lives in a column, and its URL is signed for the role reading that
    /// column (`knowledgebase/declarative-saas/decisions/033-*`).
    ///
    /// `claim_role` is the role whose write will bind the stored file into
    /// that column. A pending upload records the role that may claim it, and a
    /// produced artifact is a pending upload — so a capability that cannot name
    /// the role its process will write as cannot produce a file at all.
    pub fn new(
        attachment: &str,
        claim_role: &str,
        file_name: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Self, ConnectorFailure> {
        let invalid = |message: &'static str| {
            ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "local_artifact_invalid",
                message,
            )
        };
        if attachment.split('.').count() != 3
            || attachment
                .split('.')
                .any(|part| part.is_empty() || part.contains(char::is_whitespace))
        {
            return Err(invalid(
                "a produced artifact names the schema.table.column of its file attachment",
            ));
        }
        if file_name.is_empty()
            || file_name.contains('/')
            || file_name.contains('\\')
            || file_name.chars().any(char::is_control)
        {
            return Err(invalid("a produced artifact needs a plain file name"));
        }
        if media_type.is_empty() || !media_type.contains('/') {
            return Err(invalid("a produced artifact declares its media type"));
        }
        if claim_role.is_empty() || claim_role.chars().any(char::is_whitespace) {
            return Err(invalid(
                "a produced artifact names the role whose write will claim it",
            ));
        }
        if bytes.is_empty() {
            return Err(invalid("a produced artifact is not empty"));
        }
        Ok(Self {
            attachment: attachment.to_owned(),
            claim_role: claim_role.to_owned(),
            claim_session_key: None,
            file_name: file_name.to_owned(),
            media_type: media_type.to_owned(),
            bytes,
        })
    }

    /// Name the session identity whose write will claim these bytes.
    ///
    /// A pending upload is claimed on its role *and* its session key, and the
    /// comparison is `IS NOT DISTINCT FROM`: a row recorded with no key can
    /// only ever be claimed by a session that has none. So a capability
    /// producing a file for a role whose sessions carry an identity has to name
    /// that identity here, exactly as it has to name the role — a produced file
    /// nobody can claim is the same defect as one nobody may read (ADR 044).
    pub fn claimed_by_session(mut self, key: Option<&str>) -> Result<Self, ConnectorFailure> {
        self.claim_session_key = match key {
            // It ends up in a row and in an equality, so it is a plain value or
            // it is absent: a blank or padded one would silently mean "no
            // identity" and produce a file the intended session cannot claim.
            Some(key) => {
                if key.is_empty()
                    || key.trim() != key
                    || key.chars().any(char::is_control)
                    || key.len() > 256
                {
                    return Err(ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "local_artifact_invalid",
                        "a produced artifact's claiming session identity is a plain, non-empty \
                         value",
                    ));
                }
                Some(key.to_owned())
            }
            None => None,
        };
        Ok(self)
    }

    pub fn attachment(&self) -> &str {
        &self.attachment
    }

    /// The role whose write may bind this file into its column.
    pub fn claim_role(&self) -> &str {
        &self.claim_role
    }

    /// The session identity that write will carry, when its role has one.
    pub fn claim_session_key(&self) -> Option<&str> {
        self.claim_session_key.as_deref()
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn byte_size(&self) -> usize {
        self.bytes.len()
    }

    /// The bytes themselves, for the one caller that writes them to storage.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// What one execution produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalProduct {
    /// Typed values, bounded by the output ceiling: a normalized phone number,
    /// a rendered email's HTML and text parts.
    Value(JsonValue),
    /// Bytes for the attachment store, plus the typed metadata that goes into
    /// the activity result beside the file's identity.
    Artifact {
        artifact: LocalArtifact,
        metadata: JsonValue,
    },
}

impl LocalProduct {
    /// What the output ceiling is applied to.
    pub fn byte_size(&self) -> usize {
        match self {
            Self::Value(value) => canonical_bytes(value).len(),
            Self::Artifact { artifact, .. } => artifact.byte_size(),
        }
    }
}

/// The executor of one operation.
pub type LocalRun = fn(&LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure>;

/// The unit count an input implies, read before any work starts.
pub type LocalUnits = fn(&JsonValue) -> u64;

/// One operation of one capability.
pub struct LocalOperation {
    id: &'static str,
    version: &'static str,
    effect: Effect,
    bounds: LocalBounds,
    units: LocalUnits,
    run: LocalRun,
}

impl std::fmt::Debug for LocalOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalOperation")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("effect", &self.effect)
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

impl LocalOperation {
    pub fn declare(id: &'static str, version: &'static str) -> LocalOperationBuilder {
        LocalOperationBuilder {
            id,
            version,
            effect: None,
            bounds: None,
            units: None,
            run: None,
        }
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    pub const fn version(&self) -> &'static str {
        self.version
    }

    pub const fn effect(&self) -> &Effect {
        &self.effect
    }

    pub const fn effect_class(&self) -> EffectClass {
        self.effect.class()
    }

    pub const fn bounds(&self) -> &LocalBounds {
        &self.bounds
    }

    /// Run one execution to completion on the calling thread.
    ///
    /// The caller is expected to be a blocking-pool thread: this function does
    /// CPU work and does not yield. `ceiling` is the caller's own remaining
    /// budget — the activity's `start_to_close` — and the effective deadline is
    /// the smaller of it and the declared `cpu_deadline`, so a capability can
    /// never outlive the activity that scheduled it.
    ///
    /// Every one of the five bounds is consulted here, in the order that gives
    /// each refusal its class: the input and its unit count before any work,
    /// working memory as the implementation charges it, and the deadline and
    /// output size before anything is handed back.
    pub fn execute(
        &self,
        input: &JsonValue,
        context: &LocalContext,
        ceiling: Option<Duration>,
        stop: &StopSignal,
    ) -> Result<LocalProduct, ConnectorFailure> {
        let started = Instant::now();
        if stop.is_stopped() {
            return Err(drained());
        }
        self.bounds
            .admits_input_bytes(canonical_bytes(input).len())?;
        let units = (self.units)(input);
        self.bounds.admits_units(units)?;

        let budget = ceiling
            .unwrap_or(self.bounds.cpu_deadline())
            .min(self.bounds.cpu_deadline());
        let invocation = LocalInvocation {
            input,
            context,
            bounds: &self.bounds,
            deadline: started + budget,
            stop,
            intermediate_used: Cell::new(0),
            units_charged: Cell::new(units),
        };
        let product = (self.run)(&invocation)?;

        // Both of these discard the product rather than returning part of it:
        // an execution that ran out of time, or produced more than it declared
        // it could, produced nothing.
        if started.elapsed() > budget {
            return Err(cpu_deadline_exceeded());
        }
        self.bounds.admits_output_bytes(product.byte_size())?;
        Ok(product)
    }

    /// The registration condition of spec 018 §3, run at build time.
    ///
    /// Two renders of the declared probe, compared byte for byte. This is what
    /// makes `Pure` an admission rather than a claim: an operation that reads a
    /// clock, a random seed, a locale, or a system font produces two different
    /// results here and does not become an operation.
    pub fn prove_determinism(&self) -> Result<(), OperationError> {
        let evidence = self
            .effect
            .determinism_evidence()
            .ok_or_else(|| OperationError::new("a local operation is admitted only as Pure"))?;
        let stop = StopSignal::new();
        let context = LocalContext::builtin();
        let first = self
            .execute(evidence.probe(), context, None, &stop)
            .map_err(|_| OperationError::new("the declared determinism probe must render"))?;
        let second = self
            .execute(evidence.probe(), context, None, &stop)
            .map_err(|_| OperationError::new("the declared determinism probe must render"))?;
        if first != second {
            return Err(OperationError::new(
                "a pure operation must be deterministic: two renders of the declared probe differ",
            ));
        }
        Ok(())
    }
}

pub struct LocalOperationBuilder {
    id: &'static str,
    version: &'static str,
    effect: Option<Effect>,
    bounds: Option<LocalBounds>,
    units: Option<LocalUnits>,
    run: Option<LocalRun>,
}

impl LocalOperationBuilder {
    #[must_use]
    pub fn effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }

    #[must_use]
    pub fn bounds(mut self, bounds: LocalBounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    #[must_use]
    pub fn units(mut self, units: LocalUnits) -> Self {
        self.units = Some(units);
        self
    }

    #[must_use]
    pub fn run(mut self, run: LocalRun) -> Self {
        self.run = Some(run);
        self
    }

    /// Build the operation, and prove it before publishing it.
    pub fn build(self) -> Result<LocalOperation, OperationError> {
        validate_local_identifier(self.id)?;
        validate_semver_core(self.version)?;
        let effect = self
            .effect
            .ok_or_else(|| OperationError::new("a local operation must declare an effect class"))?;
        if effect.class() != EffectClass::Pure {
            return Err(OperationError::new(
                "a local operation is executable only as Pure; nothing else runs in this binary",
            ));
        }
        let operation = LocalOperation {
            id: self.id,
            version: self.version,
            effect,
            bounds: self
                .bounds
                .ok_or_else(|| OperationError::new("a local operation must declare its bounds"))?,
            units: self.units.ok_or_else(|| {
                OperationError::new("a local operation must declare how it counts its units")
            })?,
            run: self.run.ok_or_else(|| {
                OperationError::new("a local operation must declare its executor")
            })?,
        };
        operation.prove_determinism()?;
        Ok(operation)
    }
}

/// One capability: a reserved `local.*` name, a contract version, and its
/// operations.
#[derive(Debug)]
pub struct LocalCapability {
    name: &'static str,
    version: &'static str,
    operations: Vec<LocalOperation>,
}

impl LocalCapability {
    pub fn declare(name: &'static str, version: &'static str) -> LocalCapabilityBuilder {
        LocalCapabilityBuilder {
            name,
            version,
            operations: Vec::new(),
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn version(&self) -> &'static str {
        self.version
    }

    pub fn operations(&self) -> &[LocalOperation] {
        &self.operations
    }

    pub fn operation(&self, id: &str) -> Option<&LocalOperation> {
        self.operations
            .iter()
            .find(|operation| operation.id() == id)
    }

    /// The gate metadata validation asks before a deployment may enable an
    /// operation. It is the connector gate, answered from the declaration and
    /// with the same two refusals, because a deployment should not have to
    /// learn a second vocabulary for a capability that happens to run here.
    pub fn admit_operation(&self, id: &str) -> Result<&LocalOperation, OperationRejection> {
        let operation = self.operation(id).ok_or(OperationRejection::Undeclared)?;
        if !operation.effect().is_executable() {
            return Err(OperationRejection::InventoryOnly);
        }
        Ok(operation)
    }
}

pub struct LocalCapabilityBuilder {
    name: &'static str,
    version: &'static str,
    operations: Vec<LocalOperation>,
}

impl LocalCapabilityBuilder {
    #[must_use]
    pub fn operation(mut self, operation: LocalOperation) -> Self {
        self.operations.push(operation);
        self
    }

    pub fn build(self) -> Result<LocalCapability, OperationError> {
        if !self.name.starts_with(crate::local::LOCAL_NAMESPACE) {
            return Err(OperationError::new(
                "a local capability is named inside the reserved `local.` namespace",
            ));
        }
        validate_local_identifier(self.name)?;
        validate_semver_core(self.version)?;
        if self.operations.is_empty() {
            return Err(OperationError::new(
                "a local capability must declare at least one operation",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for operation in &self.operations {
            if !ids.insert(operation.id()) {
                return Err(OperationError::new(
                    "a local capability operation is declared more than once",
                ));
            }
        }
        Ok(LocalCapability {
            name: self.name,
            version: self.version,
            operations: self.operations,
        })
    }
}

/// The identity grammar, matching the connector name grammar so a capability
/// name and a connector name cannot be spelled by different rules.
fn validate_local_identifier(value: &str) -> Result<(), OperationError> {
    let valid = !value.is_empty()
        && value.len() <= 96
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        && value
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit());
    if !valid {
        return Err(OperationError::new(
            "a local capability name is lowercase ASCII, 1 to 96 characters, and starts and ends alphanumerically",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sdk::effect::DeterminismEvidence;

    fn run(_invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
        Ok(LocalProduct::Value(json!({ "ok": true })))
    }

    fn declaration() -> LocalOperationBuilder {
        LocalOperation::declare("thing.render", "1.0.0")
            .effect(Effect::pure(
                DeterminismEvidence::double_render(json!({}), "constant output")
                    .expect("a probe and a statement are evidence"),
            ))
            .bounds(
                LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "items", 4)
                    .expect("a complete bound declaration is valid"),
            )
            .units(|_| 1)
            .run(run)
    }

    /// Every part of the declaration is required, and a class that is not
    /// `Pure` is refused: there is no other class this binary can execute.
    #[test]
    fn a_local_operation_declaration_is_complete_and_pure() {
        assert!(declaration().build().is_ok());
        assert!(
            LocalOperation::declare("thing.render", "1.0.0")
                .bounds(
                    LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "items", 4)
                        .expect("a complete bound declaration is valid")
                )
                .units(|_| 1)
                .run(run)
                .build()
                .is_err(),
            "an operation nobody classified is not a class"
        );
        assert!(
            LocalOperation::declare("thing.render", "1.0.0")
                .effect(Effect::read_only())
                .bounds(
                    LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "items", 4)
                        .expect("a complete bound declaration is valid")
                )
                .units(|_| 1)
                .run(run)
                .build()
                .is_err(),
            "a read-only class describes a provider read, not local work"
        );
        assert!(
            LocalOperation::declare("Thing.Render", "1.0.0")
                .effect(Effect::pure(
                    DeterminismEvidence::double_render(json!({}), "constant")
                        .expect("a probe and a statement are evidence")
                ))
                .bounds(
                    LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "items", 4)
                        .expect("a complete bound declaration is valid")
                )
                .units(|_| 1)
                .run(run)
                .build()
                .is_err()
        );
    }

    /// The artifact type refuses anything that would make a stored file
    /// ambiguous: bytes belong to a declared column, under a plain name, with a
    /// media type.
    #[test]
    fn an_artifact_names_the_column_it_belongs_to() {
        assert!(
            LocalArtifact::new(
                "public.pet.photo",
                "app",
                "a.pdf",
                "application/pdf",
                vec![1]
            )
            .is_ok()
        );
        for attachment in ["", "pet.photo", "public.pet.photo.extra", "public..photo"] {
            assert!(
                LocalArtifact::new(attachment, "app", "a.pdf", "application/pdf", vec![1]).is_err(),
                "attachment {attachment} must not be accepted"
            );
        }
        assert!(
            LocalArtifact::new(
                "public.pet.photo",
                "app",
                "../../etc/passwd",
                "text/plain",
                vec![1]
            )
            .is_err()
        );
        assert!(LocalArtifact::new("public.pet.photo", "app", "a.pdf", "pdf", vec![1]).is_err());
        assert!(
            LocalArtifact::new(
                "public.pet.photo",
                "app",
                "a.pdf",
                "application/pdf",
                vec![]
            )
            .is_err()
        );
        assert!(
            LocalArtifact::new("public.pet.photo", "", "a.pdf", "application/pdf", vec![1])
                .is_err(),
            "bytes nobody may claim are bytes nobody can read"
        );
    }
}
