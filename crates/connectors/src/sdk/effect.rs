//! Effect classification — the gate that decides whether an operation may be
//! reached by a durable activity at all.
//!
//! A durable activity may be retried, or taken over after an ambiguous worker
//! loss, so an operation that cannot survive being sent twice is not
//! executable. Spec 010 §7 admits exactly two executable mutating classes, and
//! this module is where that admission is *decided* rather than reviewed: the
//! evidence a class needs is a constructor argument, so an operation that
//! mutates without admitted evidence cannot be spelled as an executable class.
//!
//! Every question about executability is asked through [`Effect::class`] and
//! [`EffectClass::is_executable`]. The third class — [`EffectClass::AtMostOnce`],
//! a provider mutation that publishes no idempotency mechanism at all — is one
//! variant here with its own evidence type, and it is executable *in the SDK's
//! terms only*: a Process reaches it solely through the per-activity opt-in of
//! `knowledgebase/declarative-saas/decisions/063-*`, and the catalog carries a
//! matching effect so process compilation can refuse an activity that did not
//! write it down.

use std::time::Duration;

use reqwest::header::HeaderName;

use crate::sdk::operation::{
    HttpMethod, OperationError, is_sdk_owned_header, validate_json_pointer,
};

/// The classes of spec 010 §7 and ADR 063, as a small copyable label.
///
/// This is what a journal, an operator message, and a Process contract see;
/// the evidence behind a class stays with the [`Effect`] that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectClass {
    ReadOnly,
    ProviderIdempotentExplicitKey,
    ProviderIdempotentNaturalMethod,
    /// Spec 018 §3: a local capability, deterministic in its input and with no
    /// external effect at all, so repeating it is indistinguishable from not
    /// having repeated it.
    Pure,
    /// ADR 063: a provider mutation for which the provider publishes no
    /// idempotency mechanism at all, admitted on evidence of that absence.
    ///
    /// Executable, but never by silence: the durable activity that references
    /// it must declare `at_most_once` and a destination for the outcome that
    /// cannot be known. The engine's promise is only that it will never send a
    /// second time — never that it sent at all.
    AtMostOnce,
    InventoryOnly,
}

impl EffectClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ProviderIdempotentExplicitKey => "provider_idempotent.explicit_key",
            Self::ProviderIdempotentNaturalMethod => "provider_idempotent.natural_method",
            Self::Pure => "pure",
            Self::AtMostOnce => "at_most_once",
            Self::InventoryOnly => "inventory_only",
        }
    }

    /// Whether a Process may reference an operation of this class.
    ///
    /// The single place the gate is answered. For [`Self::AtMostOnce`] this is
    /// a *necessary* condition rather than the whole gate: the activity's own
    /// opt-in is the other half, and process compilation refuses without it.
    pub const fn is_executable(self) -> bool {
        match self {
            Self::ReadOnly
            | Self::ProviderIdempotentExplicitKey
            | Self::ProviderIdempotentNaturalMethod
            | Self::Pure
            | Self::AtMostOnce => true,
            Self::InventoryOnly => false,
        }
    }

    /// Whether an activity referencing this class must declare the
    /// `at_most_once` opt-in and its ambiguous-outcome route.
    ///
    /// Both directions are enforced at compilation: a class that needs the
    /// opt-in and does not carry it is refused, and a class that does not need
    /// it and carries it anyway is refused too — a declaration the runtime
    /// ignores is a defect (ADR 034).
    pub const fn requires_at_most_once_opt_in(self) -> bool {
        matches!(self, Self::AtMostOnce)
    }
}

impl std::fmt::Display for EffectClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where a provider reads the stable key that makes a repeat send safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyBinding {
    Header(HeaderName),
    BodyPointer(String),
}

impl IdempotencyBinding {
    pub fn header(name: &str) -> Result<Self, OperationError> {
        if is_sdk_owned_header(name) {
            return Err(OperationError::new(
                "an idempotency binding must not name a header the SDK applies",
            ));
        }
        HeaderName::from_bytes(name.as_bytes())
            .map(Self::Header)
            .map_err(|_| OperationError::new("an idempotency header name must be static and valid"))
    }

    pub fn body_pointer(pointer: &str) -> Result<Self, OperationError> {
        validate_json_pointer(pointer)?;
        Ok(Self::BodyPointer(pointer.to_owned()))
    }

    pub const fn as_header(&self) -> Option<&HeaderName> {
        match self {
            Self::Header(name) => Some(name),
            Self::BodyPointer(_) => None,
        }
    }
}

/// The retention window a provider publishes for an idempotency key, and the
/// margin Donat keeps below it.
///
/// The margin is strictly smaller than the retention: a key that a provider
/// has already forgotten is not an idempotency key, it is a fresh request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRetention {
    scope: String,
    minimum: Duration,
    clock_safety_margin: Duration,
}

impl KeyRetention {
    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub const fn minimum(&self) -> Duration {
        self.minimum
    }

    pub const fn clock_safety_margin(&self) -> Duration {
        self.clock_safety_margin
    }
}

/// Why an operation performs no external mutation.
///
/// A `GET` says so by being a `GET`. Anything else has to be asserted, because
/// providers publish reads as `POST` all the time — a search, a quote, a
/// lookup whose selector is too big for a query string — and refusing to
/// express that would make every one of them inventory-only for no safety
/// gain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyAssertion {
    /// The method itself: a `GET` performs no external mutation.
    Method,
    /// The provider's own contract states this mutation-shaped call creates
    /// and changes nothing.
    ProviderDocumentation(String),
    /// The deploy-time declarative connector: the deployment declared the
    /// operation read-only, and the deployment is the operation's author.
    DeploymentDeclaration,
}

/// The evidence `ProviderIdempotent::ExplicitKey` is admitted on: the binding,
/// its uniqueness scope, the documented minimum retention, and a clock safety
/// margin strictly smaller than that retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitKeyEvidence {
    binding: IdempotencyBinding,
    retention: KeyRetention,
    citation: String,
}

impl ExplicitKeyEvidence {
    /// The one constructor: every piece of the evidence, or none of it.
    ///
    /// A hand-written connector passes the provider's statement; the
    /// declarative connector passes the deployment's declared source record.
    /// A missing scope, a missing citation, or an out-of-order margin is
    /// refused here rather than reviewed later.
    pub fn documented(
        binding: IdempotencyBinding,
        scope: &str,
        minimum_retention: Duration,
        clock_safety_margin: Duration,
        citation: &str,
    ) -> Result<Self, OperationError> {
        if scope.trim().is_empty() {
            return Err(OperationError::new(
                "an explicit idempotency key requires a documented uniqueness scope",
            ));
        }
        if citation.trim().is_empty() {
            return Err(OperationError::new(
                "an explicit idempotency key requires the provider documentation statement",
            ));
        }
        if minimum_retention.is_zero() || clock_safety_margin.is_zero() {
            return Err(OperationError::new(
                "an explicit idempotency key requires a positive retention and clock margin",
            ));
        }
        if clock_safety_margin >= minimum_retention {
            return Err(OperationError::new(
                "an idempotency clock safety margin must be strictly smaller than the documented retention",
            ));
        }
        Ok(Self {
            binding,
            retention: KeyRetention {
                scope: scope.to_owned(),
                minimum: minimum_retention,
                clock_safety_margin,
            },
            citation: citation.to_owned(),
        })
    }

    pub const fn binding(&self) -> &IdempotencyBinding {
        &self.binding
    }

    pub const fn retention(&self) -> &KeyRetention {
        &self.retention
    }

    pub fn citation(&self) -> &str {
        &self.citation
    }
}

/// The evidence `Pure` is admitted on: one operation input to render twice,
/// and the statement of what makes the output a function of that input alone.
///
/// Spec 018 §3 makes determinism a *registration condition* rather than a
/// documented expectation, and this type is why it can be one: a class that
/// carries the probe it is proven on can be proven at the place it is
/// declared. [`crate::local`] renders every registered `Pure` operation twice
/// on this probe and refuses to publish a capability whose two renders differ,
/// so "anything with a clock, a random seed, or a system font lookup is not
/// `Pure`" is enforced by the build rather than by review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterminismEvidence {
    probe: serde_json::Value,
    statement: String,
}

impl DeterminismEvidence {
    /// The one constructor: the input the double render runs on, and why the
    /// operation has no hidden input besides it.
    pub fn double_render(
        probe: serde_json::Value,
        statement: &str,
    ) -> Result<Self, OperationError> {
        if !probe.is_object() {
            return Err(OperationError::new(
                "a determinism probe is one operation input, which is a JSON object",
            ));
        }
        if statement.trim().is_empty() {
            return Err(OperationError::new(
                "a pure operation must state what makes its output a function of its input",
            ));
        }
        Ok(Self {
            probe,
            statement: statement.to_owned(),
        })
    }

    /// The input the registration-time double render uses.
    pub const fn probe(&self) -> &serde_json::Value {
        &self.probe
    }

    pub fn statement(&self) -> &str {
        &self.statement
    }
}

/// How the absence of an idempotency mechanism was established.
///
/// No provider publishes the sentence "this endpoint has no idempotency key",
/// so a negative is established by what *was* read, and the two forms are not
/// equally strong. `crates/connectors/src/providers/INVENTORY.md` records which
/// one each operation stands on, and this type is the same distinction with a
/// compiler behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsenceSearch {
    /// The endpoint's own reference page enumerates its complete request
    /// contract — headers, body, query — and no idempotency key,
    /// client-supplied request identifier, or deduplication behaviour appears
    /// in it, nor anywhere else in that provider's API documentation.
    PublishedContract,
    /// The provider publishes a machine-readable description of the API
    /// (OpenAPI, a discovery document, a GraphQL schema) and the term does not
    /// occur in it for this operation. The stronger of the two.
    MachineReadableDescription,
}

impl AbsenceSearch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PublishedContract => "published_contract",
            Self::MachineReadableDescription => "machine_readable_description",
        }
    }
}

/// The evidence [`EffectClass::AtMostOnce`] is admitted on: what was searched,
/// and what a second send would leave behind.
///
/// It is the mirror image of [`ExplicitKeyEvidence`]. That class cites a
/// mechanism the provider publishes; this one cites the search that found none,
/// so a reviewer reading the module sees *what was read* rather than a bare
/// assertion that nothing exists. The consequence sentence is required because
/// it is the thing an operator is accepting when they write the opt-in: this is
/// what happens if the engine is wrong about having sent nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoIdempotencyEvidence {
    search: AbsenceSearch,
    searched: String,
    repeat_produces: String,
}

impl NoIdempotencyEvidence {
    /// The one constructor: the kind of search, the documentation it covered,
    /// and what a repeat produces. All three, or none of it.
    pub fn searched(
        search: AbsenceSearch,
        searched: &str,
        repeat_produces: &str,
    ) -> Result<Self, OperationError> {
        if searched.trim().is_empty() {
            return Err(OperationError::new(
                "an at-most-once operation must record which provider documentation was searched",
            ));
        }
        if repeat_produces.trim().is_empty() {
            return Err(OperationError::new(
                "an at-most-once operation must record what a second send would produce",
            ));
        }
        Ok(Self {
            search,
            searched: searched.to_owned(),
            repeat_produces: repeat_produces.to_owned(),
        })
    }

    pub const fn search(&self) -> AbsenceSearch {
        self.search
    }

    pub fn searched_documentation(&self) -> &str {
        &self.searched
    }

    pub fn repeat_produces(&self) -> &str {
        &self.repeat_produces
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EffectKind {
    ReadOnly(ReadOnlyAssertion),
    ExplicitKey(ExplicitKeyEvidence),
    NaturalMethod { citation: String },
    Pure(DeterminismEvidence),
    AtMostOnce(NoIdempotencyEvidence),
    InventoryOnly { reason: String },
}

/// One operation's effect class and the evidence it was admitted on.
///
/// As with [`crate::sdk::auth::AuthPlan`], the representation is private: a
/// provider module selects one of these four, and a fifth is an edit to this
/// file with its own test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    kind: EffectKind,
}

impl Effect {
    /// A read the method itself proves: a `GET`.
    pub const fn read_only() -> Self {
        Self {
            kind: EffectKind::ReadOnly(ReadOnlyAssertion::Method),
        }
    }

    /// A read the provider publishes as a mutation-shaped request — a search, a
    /// quote, a lookup with a body — where the provider's contract states the
    /// call creates and changes nothing.
    ///
    /// The statement is required because the method no longer proves it: this
    /// is the one place a non-`GET` becomes executable without idempotency
    /// evidence, and what stands behind it is a citation a reviewer can check.
    pub fn read_only_documented(statement: &str) -> Result<Self, OperationError> {
        if statement.trim().is_empty() {
            return Err(OperationError::new(
                "a mutation-shaped read requires the provider statement that it changes nothing",
            ));
        }
        Ok(Self {
            kind: EffectKind::ReadOnly(ReadOnlyAssertion::ProviderDocumentation(
                statement.to_owned(),
            )),
        })
    }

    /// The same, declared by a deployment for an operation the deployment
    /// itself authored (the deploy-time declarative connector).
    pub const fn read_only_declared_by_deployment() -> Self {
        Self {
            kind: EffectKind::ReadOnly(ReadOnlyAssertion::DeploymentDeclaration),
        }
    }

    /// `ProviderIdempotent::ExplicitKey`: the provider documents a key it
    /// deduplicates on, and the connector binds it.
    pub const fn provider_idempotent_explicit_key(evidence: ExplicitKeyEvidence) -> Self {
        Self {
            kind: EffectKind::ExplicitKey(evidence),
        }
    }

    /// `ProviderIdempotent::NaturalMethod`: a `PUT` or `DELETE` against a fixed
    /// resource identity whose repeat-safe semantics the provider documents.
    ///
    /// The method itself is checked where the operation is built, because a
    /// `POST` cannot become repeat-safe by being described as one.
    pub fn provider_idempotent_natural_method(citation: &str) -> Result<Self, OperationError> {
        if citation.trim().is_empty() {
            return Err(OperationError::new(
                "a naturally idempotent operation requires the provider documentation statement",
            ));
        }
        Ok(Self {
            kind: EffectKind::NaturalMethod {
                citation: citation.to_owned(),
            },
        })
    }

    /// `Pure`: a local capability (spec 018), whose executor is in this binary
    /// and whose output is a function of its declared input.
    ///
    /// The evidence is the probe the double render runs on, because the class
    /// is only safe while the determinism holds — see [`DeterminismEvidence`].
    pub const fn pure(evidence: DeterminismEvidence) -> Self {
        Self {
            kind: EffectKind::Pure(evidence),
        }
    }

    /// [`EffectClass::AtMostOnce`]: a provider mutation for which the provider
    /// publishes no idempotency mechanism, admitted on evidence of that
    /// absence.
    ///
    /// This class buys nothing from the provider — the provider would accept a
    /// second send happily — so the whole of its safety is Donat's refusal to
    /// make one, and a Process reaches it only by declaring `at_most_once` on
    /// the activity together with a destination for an outcome nobody can know.
    /// The evidence is what a reviewer checks: the search that found no key,
    /// and the consequence of being wrong.
    pub const fn at_most_once(evidence: NoIdempotencyEvidence) -> Self {
        Self {
            kind: EffectKind::AtMostOnce(evidence),
        }
    }

    /// Anything else: declared, typed, and tested, but never executable.
    pub fn inventory_only(reason: &str) -> Result<Self, OperationError> {
        if reason.trim().is_empty() {
            return Err(OperationError::new(
                "an inventory-only operation must record why it is not executable",
            ));
        }
        Ok(Self {
            kind: EffectKind::InventoryOnly {
                reason: reason.to_owned(),
            },
        })
    }

    /// Why a read-only operation was admitted as one.
    pub const fn read_only_assertion(&self) -> Option<&ReadOnlyAssertion> {
        match &self.kind {
            EffectKind::ReadOnly(assertion) => Some(assertion),
            _ => None,
        }
    }

    /// The probe and statement a `Pure` operation was admitted on.
    pub const fn determinism_evidence(&self) -> Option<&DeterminismEvidence> {
        match &self.kind {
            EffectKind::Pure(evidence) => Some(evidence),
            _ => None,
        }
    }

    /// The search and consequence an at-most-once operation was admitted on.
    pub const fn no_idempotency_evidence(&self) -> Option<&NoIdempotencyEvidence> {
        match &self.kind {
            EffectKind::AtMostOnce(evidence) => Some(evidence),
            _ => None,
        }
    }

    pub const fn class(&self) -> EffectClass {
        match &self.kind {
            EffectKind::ReadOnly(_) => EffectClass::ReadOnly,
            EffectKind::ExplicitKey(_) => EffectClass::ProviderIdempotentExplicitKey,
            EffectKind::NaturalMethod { .. } => EffectClass::ProviderIdempotentNaturalMethod,
            EffectKind::Pure(_) => EffectClass::Pure,
            EffectKind::AtMostOnce(_) => EffectClass::AtMostOnce,
            EffectKind::InventoryOnly { .. } => EffectClass::InventoryOnly,
        }
    }

    pub const fn is_executable(&self) -> bool {
        self.class().is_executable()
    }

    /// The key binding a durable activity writes its stable key into.
    pub const fn idempotency_binding(&self) -> Option<&IdempotencyBinding> {
        match &self.kind {
            EffectKind::ExplicitKey(evidence) => Some(evidence.binding()),
            EffectKind::ReadOnly(_)
            | EffectKind::NaturalMethod { .. }
            | EffectKind::Pure(_)
            | EffectKind::AtMostOnce(_)
            | EffectKind::InventoryOnly { .. } => None,
        }
    }

    pub const fn explicit_key_evidence(&self) -> Option<&ExplicitKeyEvidence> {
        match &self.kind {
            EffectKind::ExplicitKey(evidence) => Some(evidence),
            _ => None,
        }
    }

    /// Why an inventory-only operation is not executable.
    pub fn inventory_reason(&self) -> Option<&str> {
        match &self.kind {
            EffectKind::InventoryOnly { reason } => Some(reason),
            _ => None,
        }
    }

    /// The method half of the gate, applied where an operation is built.
    ///
    /// A mutation-shaped method cannot be called a read on the strength of its
    /// method alone, and repeat-safety by method is admitted only for the two
    /// methods HTTP defines it for.
    pub(in crate::sdk) fn admit_method(&self, method: HttpMethod) -> Result<(), OperationError> {
        match &self.kind {
            EffectKind::ReadOnly(ReadOnlyAssertion::Method) if method.mutates() => {
                Err(OperationError::new(
                    "a mutating method is not read-only by its method; assert it with the provider statement, declare idempotency evidence, or declare InventoryOnly",
                ))
            }
            EffectKind::ReadOnly(
                ReadOnlyAssertion::ProviderDocumentation(_)
                | ReadOnlyAssertion::DeploymentDeclaration,
            ) if !method.mutates() => Err(OperationError::new(
                "a GET is read-only by its method; it needs no assertion",
            )),
            EffectKind::ExplicitKey(_) if !method.mutates() => Err(OperationError::new(
                "an operation that performs no mutation is ReadOnly, not ProviderIdempotent",
            )),
            EffectKind::NaturalMethod { .. }
                if !matches!(method, HttpMethod::Put | HttpMethod::Delete) =>
            {
                Err(OperationError::new(
                    "NaturalMethod idempotency is admitted only for PUT and DELETE",
                ))
            }
            // A local capability has no origin, no credential, and no request
            // to render; a class that classified one would be describing work
            // that does not exist.
            EffectKind::Pure(_) => Err(OperationError::new(
                "a pure effect classifies a local capability, which renders no HTTP request",
            )),
            // At-most-once exists to bound a *mutation* nobody can replay. A
            // GET has nothing to bound, and classifying one this way would make
            // a Process declare an opt-in for a risk it never ran.
            EffectKind::AtMostOnce(_) if !method.mutates() => Err(OperationError::new(
                "an operation that performs no mutation is ReadOnly, not AtMostOnce",
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn documented_evidence() -> ExplicitKeyEvidence {
        ExplicitKeyEvidence::documented(
            IdempotencyBinding::header("Idempotency-Key").expect("a static header name is valid"),
            "account",
            Duration::from_secs(24 * 60 * 60),
            Duration::from_secs(300),
            "the provider documents Idempotency-Key and a 24 hour retention",
        )
        .expect("complete evidence is admitted")
    }

    #[test]
    fn an_explicit_key_needs_every_piece_of_its_evidence() {
        let binding = || {
            IdempotencyBinding::header("Idempotency-Key").expect("a static header name is valid")
        };
        let day = Duration::from_secs(24 * 60 * 60);

        assert!(
            ExplicitKeyEvidence::documented(binding(), "", day, Duration::from_secs(1), "cited")
                .is_err(),
            "an idempotency key without a uniqueness scope is not evidence"
        );
        assert!(
            ExplicitKeyEvidence::documented(binding(), "account", day, Duration::from_secs(1), " ")
                .is_err(),
            "an idempotency key without the provider statement is not evidence"
        );
        assert!(
            ExplicitKeyEvidence::documented(
                binding(),
                "account",
                Duration::ZERO,
                Duration::from_secs(1),
                "cited"
            )
            .is_err()
        );
        assert!(
            ExplicitKeyEvidence::documented(binding(), "account", day, day, "cited").is_err(),
            "a margin equal to the retention leaves no margin at all"
        );
        assert!(
            ExplicitKeyEvidence::documented(binding(), "account", day, day * 2, "cited").is_err()
        );
        assert!(
            ExplicitKeyEvidence::documented(
                binding(),
                "account",
                day,
                day - Duration::from_secs(1),
                "cited"
            )
            .is_ok(),
            "one second under the documented retention is admitted"
        );
    }

    #[test]
    fn an_idempotency_binding_is_static_and_never_a_header_the_sdk_applies() {
        for name in ["Authorization", "content-length", "Host"] {
            assert!(IdempotencyBinding::header(name).is_err(), "{name}");
        }
        assert!(IdempotencyBinding::header("X-Key-{tenant}").is_err());
        assert!(IdempotencyBinding::body_pointer("key").is_err());
        assert!(IdempotencyBinding::body_pointer("/key").is_ok());
    }

    #[test]
    fn a_class_carries_its_evidence_and_its_executability() {
        let explicit = Effect::provider_idempotent_explicit_key(documented_evidence());
        assert_eq!(explicit.class(), EffectClass::ProviderIdempotentExplicitKey);
        assert!(explicit.is_executable());
        assert!(explicit.idempotency_binding().is_some());
        assert_eq!(
            explicit
                .explicit_key_evidence()
                .map(|evidence| evidence.retention().scope()),
            Some("account")
        );

        let natural = Effect::provider_idempotent_natural_method("the provider documents PUT")
            .expect("a cited statement is evidence");
        assert_eq!(
            natural.class(),
            EffectClass::ProviderIdempotentNaturalMethod
        );
        assert!(natural.is_executable());
        assert!(
            natural.idempotency_binding().is_none(),
            "a naturally idempotent operation carries no key"
        );
        assert!(Effect::provider_idempotent_natural_method("  ").is_err());

        let inventory = Effect::inventory_only("the provider publishes no idempotency key")
            .expect("a recorded reason is required");
        assert_eq!(inventory.class(), EffectClass::InventoryOnly);
        assert!(!inventory.is_executable());
        assert_eq!(
            inventory.inventory_reason(),
            Some("the provider publishes no idempotency key")
        );
        assert!(Effect::inventory_only("").is_err());

        assert!(Effect::read_only().is_executable());
        assert_eq!(EffectClass::InventoryOnly.as_str(), "inventory_only");
    }

    /// `Pure` is the local-capability class, and its evidence is the probe a
    /// registration-time double render runs on. Without a probe and a
    /// statement of what makes the operation a function of its input, there is
    /// nothing to double-render and nothing to review.
    #[test]
    fn a_pure_class_carries_the_probe_its_determinism_is_proven_on() {
        let evidence = DeterminismEvidence::double_render(
            serde_json::json!({ "value": "x" }),
            "no clock, no random seed, no environment lookup",
        )
        .expect("a probe and a statement are evidence");
        let pure = Effect::pure(evidence);

        assert_eq!(pure.class(), EffectClass::Pure);
        assert!(pure.is_executable(), "a pure operation is safe to repeat");
        assert_eq!(EffectClass::Pure.as_str(), "pure");
        assert!(
            pure.idempotency_binding().is_none(),
            "a pure operation reaches no provider, so it carries no key"
        );
        assert_eq!(
            pure.determinism_evidence().map(DeterminismEvidence::probe),
            Some(&serde_json::json!({ "value": "x" }))
        );

        assert!(
            DeterminismEvidence::double_render(serde_json::json!({ "value": "x" }), "  ").is_err(),
            "a class admitted on determinism must say what makes it deterministic"
        );
        assert!(
            DeterminismEvidence::double_render(serde_json::json!("x"), "cited").is_err(),
            "a probe is one operation input, which is an object"
        );

        // The gate in the other direction: a pure effect classifies work this
        // binary performs, so it can never be attached to an HTTP operation.
        for method in [HttpMethod::Get, HttpMethod::Post, HttpMethod::Put] {
            assert!(
                Effect::pure(
                    DeterminismEvidence::double_render(serde_json::json!({}), "cited")
                        .expect("a probe and a statement are evidence"),
                )
                .admit_method(method)
                .is_err(),
                "a pure effect must not classify an HTTP request: {method:?}"
            );
        }
    }

    /// ADR 063. The third executable class carries evidence of an *absence*,
    /// and it is admitted on the same two things `INVENTORY.md` already
    /// records: the search that found no key, and what a second send produces.
    /// Silence does not reach it, and neither does a `GET`.
    #[test]
    fn an_at_most_once_class_carries_the_search_that_found_no_key() {
        let evidence = NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "the endpoint's own reference page enumerates its complete request contract",
            "a second delivered email with a new message id",
        )
        .expect("a search and a consequence are evidence");
        let effect = Effect::at_most_once(evidence);

        assert_eq!(effect.class(), EffectClass::AtMostOnce);
        assert_eq!(EffectClass::AtMostOnce.as_str(), "at_most_once");
        assert!(
            effect.is_executable(),
            "the class is reachable; the activity's opt-in is the other half of the gate"
        );
        assert!(
            EffectClass::AtMostOnce.requires_at_most_once_opt_in(),
            "no other class may carry the opt-in, and this one may not omit it"
        );
        for class in [
            EffectClass::ReadOnly,
            EffectClass::ProviderIdempotentExplicitKey,
            EffectClass::ProviderIdempotentNaturalMethod,
            EffectClass::Pure,
            EffectClass::InventoryOnly,
        ] {
            assert!(!class.requires_at_most_once_opt_in(), "{class}");
        }
        assert!(
            effect.idempotency_binding().is_none(),
            "the class exists precisely because there is no key to bind"
        );
        assert_eq!(
            effect
                .no_idempotency_evidence()
                .map(NoIdempotencyEvidence::repeat_produces),
            Some("a second delivered email with a new message id")
        );
        assert_eq!(
            effect
                .no_idempotency_evidence()
                .map(|evidence| evidence.search().as_str()),
            Some("published_contract")
        );

        assert!(
            NoIdempotencyEvidence::searched(
                AbsenceSearch::PublishedContract,
                "  ",
                "a second email"
            )
            .is_err(),
            "a negative established by nothing is not evidence"
        );
        assert!(
            NoIdempotencyEvidence::searched(
                AbsenceSearch::MachineReadableDescription,
                "the published OpenAPI document",
                "\t"
            )
            .is_err(),
            "the consequence of being wrong is what the opt-in accepts, so it is required"
        );

        assert!(
            effect.admit_method(HttpMethod::Get).is_err(),
            "a GET has no send to bound; it is ReadOnly"
        );
        for method in [
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
        ] {
            assert!(effect.admit_method(method).is_ok(), "{method:?}");
        }
    }

    /// A read-only class records *why* it is one, and the three answers are
    /// not interchangeable: only a `GET` is read-only by its method.
    #[test]
    fn a_read_only_class_records_what_makes_it_one() {
        assert_eq!(
            Effect::read_only().read_only_assertion(),
            Some(&ReadOnlyAssertion::Method)
        );
        assert_eq!(
            Effect::read_only_documented("the provider documents a search as creating nothing")
                .expect("a cited statement is an assertion")
                .read_only_assertion(),
            Some(&ReadOnlyAssertion::ProviderDocumentation(
                "the provider documents a search as creating nothing".to_owned()
            ))
        );
        assert_eq!(
            Effect::read_only_declared_by_deployment().read_only_assertion(),
            Some(&ReadOnlyAssertion::DeploymentDeclaration)
        );
        assert!(
            Effect::read_only_documented("   ").is_err(),
            "an empty statement asserts nothing"
        );
        assert_eq!(
            Effect::read_only().class(),
            Effect::read_only_declared_by_deployment().class(),
            "all three are the same class; only their evidence differs"
        );
        assert!(Effect::read_only().admit_method(HttpMethod::Post).is_err());
        assert!(
            Effect::read_only_documented("cited")
                .expect("a cited statement is an assertion")
                .admit_method(HttpMethod::Get)
                .is_err(),
            "a GET needs no assertion, so declaring one is a defect rather than caution"
        );
        assert!(
            Effect::read_only_documented("cited")
                .expect("a cited statement is an assertion")
                .admit_method(HttpMethod::Post)
                .is_ok()
        );
    }
}
