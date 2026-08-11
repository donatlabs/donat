//! Local capabilities (spec 018): the work a deployment repeats that has no
//! provider at all.
//!
//! Rendering an invoice, building a responsive email, producing a spreadsheet,
//! normalizing a phone number — none of these has an origin, a credential, a
//! network, or idempotency evidence, because each is a function of its input.
//! A local capability is therefore *not* a [`crate::sdk::Connector`]: it
//! declares no origin and no credential, and its operations carry no method,
//! path, or status contract. What it does declare is what a connector cannot:
//! the executor that runs in this binary, and the five bounds that executor
//! runs inside.
//!
//! Three properties are structural here rather than reviewed.
//!
//! *Determinism is a registration condition.* Spec 018 §3 admits `Pure` only
//! while "same input bytes produce byte-identical output" holds, so
//! [`LocalOperation`] renders its declared probe twice at build time and
//! refuses to publish an operation whose two renders differ. A capability with
//! a clock, a random seed, or a system font lookup does not reach the table.
//!
//! *All five bounds are mandatory.* [`LocalBounds::declare`] takes every one of
//! them as a positional argument, so an operation cannot be declared with four.
//! [`LocalOperation::execute`] consults all five, because a bound the runtime
//! ignores is a defect (`knowledgebase/declarative-saas/decisions/034-*`).
//!
//! *Bytes never come back inline.* A capability that produces bytes returns a
//! [`LocalArtifact`], which names the attachment column its bytes belong to and
//! is not serializable. The activity result carries the stored file's identity
//! and typed metadata, exactly as an uploaded attachment does
//! (`knowledgebase/declarative-saas/decisions/033-*`).
//!
//! Execution itself is synchronous and CPU-bound on purpose: the caller runs it
//! on the blocking pool and hands it a [`StopSignal`] wired to the deployment's
//! shutdown token, so a rolling deployment drains rather than abandoning work
//! (`knowledgebase/operations/decisions/001-*`).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Map as JsonMap, Value as JsonValue};

pub mod bounds;
pub mod capability;
pub mod code;
pub mod context;
pub mod document;
pub mod echo;
pub mod image;
pub mod ingest;
pub mod media;
pub mod recurrence;

pub use bounds::{LocalBounds, MAX_CPU_DEADLINE};
pub use capability::{
    LocalArtifact, LocalCapability, LocalCapabilityBuilder, LocalInvocation, LocalOperation,
    LocalOperationBuilder, LocalProduct, LocalRun, LocalUnits, StopSignal,
};
pub use context::LocalContext;
pub use document::{
    DocumentKind, DocumentTemplate, DocumentTemplateSet, DocumentTemplateSpec, TemplateRejection,
};
pub use ingest::{
    IngestColumnSpec, IngestKind, IngestRejection, IngestSchemaSet, IngestSchemaSpec,
    RowErrorPolicy, SourceFile,
};
pub use media::{
    CodeTemplate, CodeTemplateSpec, ImageTarget, ImageTargetSpec, MediaCatalog, MediaRejection,
};
pub use recurrence::{
    DstPolicy, RecurrencePolicy, RecurrencePolicySet, RecurrencePolicySpec, RecurrenceRejection,
    RepeatedTime, SkippedTime,
};

/// The reserved connector namespace. A deployment may not name anything else
/// `local.*`, and a `local.*` instance may not declare an origin, a base URL, a
/// header, or a credential.
pub const LOCAL_NAMESPACE: &str = "local.";

/// The complete table of capabilities compiled into this binary.
///
/// Building it runs every operation's registration checks, including the
/// double render: a capability that cannot prove its determinism panics here,
/// at startup, rather than producing two different invoices in production.
pub fn capabilities() -> &'static [LocalCapability] {
    static TABLE: LazyLock<Vec<LocalCapability>> = LazyLock::new(|| {
        vec![
            document::capability(),
            echo::capability(),
            code::capability(),
            image::capability(),
            ingest::capability(),
            recurrence::capability(),
        ]
    });
    &TABLE
}

/// The capability a `local.*` connector name selects, if any.
pub fn capability(name: &str) -> Option<&'static LocalCapability> {
    capabilities()
        .iter()
        .find(|capability| capability.name() == name)
}

/// The canonical bytes of one operation input.
///
/// Object key order is not part of an input's identity — the same input written
/// in another order is the same input — so the size a bound is applied to, and
/// the bytes a double render compares, are both taken from this form.
pub fn canonical_bytes(value: &JsonValue) -> Vec<u8> {
    fn canonical(value: &JsonValue) -> JsonValue {
        match value {
            JsonValue::Object(object) => BTreeMap::from_iter(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value))),
            )
            .into_iter()
            .collect::<JsonMap<String, JsonValue>>()
            .into(),
            JsonValue::Array(values) => JsonValue::Array(values.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }

    serde_json::to_vec(&canonical(value)).expect("a JSON value always serializes")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::local::context::LocalContext;
    use crate::sdk::effect::{DeterminismEvidence, Effect, EffectClass};
    use crate::sdk::errors::ConnectorErrorClass;

    /// Spec 018 §8 `pure_effect_requires_determinism`.
    ///
    /// Every registered `Pure` operation renders twice from one input and
    /// produces identical bytes — and a capability whose double render differs
    /// cannot be registered at all, which is the half that makes the class
    /// safe rather than merely documented.
    #[test]
    fn pure_effect_requires_determinism() {
        let mut operations = 0;
        for capability in capabilities() {
            for operation in capability.operations() {
                assert_eq!(
                    operation.effect_class(),
                    EffectClass::Pure,
                    "{}.{} is registered as a local capability",
                    capability.name(),
                    operation.id()
                );
                let evidence = operation
                    .effect()
                    .determinism_evidence()
                    .expect("a pure operation carries the probe it was admitted on");
                let stop = StopSignal::new();
                let first = operation
                    .execute(evidence.probe(), LocalContext::builtin(), None, &stop)
                    .expect("the declared probe renders");
                let second = operation
                    .execute(evidence.probe(), LocalContext::builtin(), None, &stop)
                    .expect("the declared probe renders again");
                assert_eq!(
                    first,
                    second,
                    "{}.{} renders differently from one input",
                    capability.name(),
                    operation.id()
                );
                operations += 1;
            }
        }
        assert!(operations > 0, "the table must carry a capability");

        // The registration condition itself: an operation whose output depends
        // on anything but its input does not build, so it never reaches a
        // table, a metadata validation, or a process.
        static RENDERS: AtomicU64 = AtomicU64::new(0);
        fn nondeterministic(
            _invocation: &LocalInvocation<'_>,
        ) -> Result<LocalProduct, crate::sdk::ConnectorFailure> {
            Ok(LocalProduct::Value(
                json!({ "render": RENDERS.fetch_add(1, Ordering::SeqCst) }),
            ))
        }

        let refused = LocalOperation::declare("counter.render", "1.0.0")
            .effect(Effect::pure(
                DeterminismEvidence::double_render(json!({}), "it is not, and that is the point")
                    .expect("a probe and a statement are evidence"),
            ))
            .bounds(test_bounds())
            .units(|_| 1)
            .run(nondeterministic)
            .build()
            .expect_err("an operation that renders twice differently is not Pure");
        assert!(
            refused.message().contains("deterministic"),
            "the refusal must name what failed: {refused}"
        );
    }

    /// Spec 018 §8 `local_bounds_are_declared_and_exact`.
    ///
    /// Every operation declares all five bounds; each accepts its exact
    /// boundary and rejects one over with the right class — `validation` for a
    /// limit the input was already over, `timeout` for the deadline — and
    /// nothing partial is ever returned.
    #[test]
    fn local_bounds_are_declared_and_exact() {
        // 1. Declared. All five, on every registered operation.
        for capability in capabilities() {
            for operation in capability.operations() {
                let bounds = operation.bounds();
                assert!(!bounds.cpu_deadline().is_zero());
                assert!(bounds.cpu_deadline() <= MAX_CPU_DEADLINE);
                assert!(bounds.max_input_bytes() > 0);
                assert!(bounds.max_output_bytes() > 0);
                assert!(bounds.max_intermediate_bytes() > 0);
                assert!(bounds.max_units() > 0);
                assert!(
                    !bounds.unit().is_empty(),
                    "{}.{} must name what it counts",
                    capability.name(),
                    operation.id()
                );
            }
        }
        // A declaration missing any one of them does not typecheck; a
        // declaration with a zero, or a deadline over the ceiling, is refused.
        assert!(
            LocalBounds::declare(Duration::ZERO, 64, 64, 64, "items", 1).is_err(),
            "a deadline of zero is not a deadline"
        );
        assert!(LocalBounds::declare(Duration::from_secs(1), 0, 64, 64, "items", 1).is_err());
        assert!(LocalBounds::declare(Duration::from_secs(1), 64, 0, 64, "items", 1).is_err());
        assert!(LocalBounds::declare(Duration::from_secs(1), 64, 64, 0, "items", 1).is_err());
        assert!(LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "", 1).is_err());
        assert!(LocalBounds::declare(Duration::from_secs(1), 64, 64, 64, "items", 0).is_err());
        assert!(
            LocalBounds::declare(
                MAX_CPU_DEADLINE + Duration::from_secs(1),
                64,
                64,
                64,
                "items",
                1
            )
            .is_err(),
            "a bound has to be bounded itself"
        );

        // 2. Exact, one limit at a time.
        let bounds = test_bounds();
        assert_eq!(bounds.admits_input_bytes(128), Ok(()));
        assert_eq!(
            bounds.admits_input_bytes(129).unwrap_err().class(),
            ConnectorErrorClass::Validation
        );
        assert_eq!(bounds.admits_units(8), Ok(()));
        assert_eq!(
            bounds.admits_units(9).unwrap_err().class(),
            ConnectorErrorClass::Validation
        );
        assert_eq!(bounds.admits_output_bytes(64), Ok(()));
        assert_eq!(
            bounds.admits_output_bytes(65).unwrap_err().class(),
            ConnectorErrorClass::Validation
        );
        assert_eq!(bounds.admits_intermediate_bytes(256), Ok(()));
        assert_eq!(
            bounds.admits_intermediate_bytes(257).unwrap_err().class(),
            ConnectorErrorClass::Validation
        );
        assert_eq!(bounds.admits_elapsed(Duration::from_millis(200)), Ok(()));
        assert_eq!(
            bounds
                .admits_elapsed(Duration::from_millis(200) + Duration::from_nanos(1))
                .unwrap_err()
                .class(),
            ConnectorErrorClass::Timeout,
            "a deadline reached is a timeout, not a validation failure"
        );

        // 3. Consulted. One execution per bound, through the real path.
        let operation = probe_operation();
        let stop = StopSignal::new();
        let run =
            |input: JsonValue| operation.execute(&input, LocalContext::builtin(), None, &stop);

        // The input ceiling is applied to the canonical input, before any work.
        let at_limit = padded_input(128);
        assert_eq!(canonical_bytes(&at_limit).len(), 128);
        assert!(
            run(at_limit).is_ok(),
            "the exact input boundary is admitted"
        );
        let over = padded_input(129);
        let failure = run(over).expect_err("one byte over the input ceiling is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_input_too_large");

        // Units, counted from the input before work starts.
        assert!(run(json!({ "units": 8, "output": 1 })).is_ok());
        let failure = run(json!({ "units": 9, "output": 1 })).expect_err("nine items is one over");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_units_exceeded");

        // Working memory, charged by the implementation as it allocates.
        assert!(run(json!({ "units": 1, "output": 1, "reserve": 256 })).is_ok());
        let failure = run(json!({ "units": 1, "output": 1, "reserve": 257 }))
            .expect_err("one byte over the working-memory ceiling is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_intermediate_too_large");

        // The produced artifact, measured before it is handed back.
        assert!(run(json!({ "units": 1, "output": 64 })).is_ok());
        let failure =
            run(json!({ "units": 1, "output": 65 })).expect_err("a 65 byte artifact is one over");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_output_too_large");

        // The deadline. What comes back is a timeout and nothing else: a
        // capability that ran out of time returns no partial output.
        let failure = run(json!({ "units": 1, "output": 1, "spin_ms": 800 }))
            .expect_err("work past the deadline is a timeout");
        assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
        assert_eq!(failure.code(), "local_cpu_deadline_exceeded");
    }

    /// A stopped execution ends where it is, with no output: the drain half of
    /// spec 018 §4, proven here on the capability itself and in the server on
    /// the deployment's shutdown token.
    #[test]
    fn a_stopped_execution_returns_no_output() {
        let operation = probe_operation();
        let stop = StopSignal::new();
        stop.stop();
        let failure = operation
            .execute(
                &json!({ "units": 1, "output": 1, "spin_ms": 50 }),
                LocalContext::builtin(),
                None,
                &stop,
            )
            .expect_err("a stopped execution does not produce output");
        assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
        assert_eq!(failure.code(), "local_capability_drained");
    }

    /// The caller's own ceiling — the activity's `start_to_close` — is applied
    /// alongside the declared one, and the smaller of the two wins.
    #[test]
    fn the_effective_deadline_is_the_smaller_of_the_two() {
        let operation = probe_operation();
        let stop = StopSignal::new();
        let failure = operation
            .execute(
                &json!({ "units": 1, "output": 1, "spin_ms": 300 }),
                LocalContext::builtin(),
                Some(Duration::from_millis(20)),
                &stop,
            )
            .expect_err("a caller ceiling under the declared deadline still bounds the work");
        assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
    }

    /// The table is the whole world: every entry sits in the reserved
    /// namespace, is reachable by the name it carries, and declares no
    /// operation twice.
    #[test]
    fn the_capability_table_is_static_and_reserved() {
        for registered in capabilities() {
            assert!(
                registered.name().starts_with(LOCAL_NAMESPACE),
                "{} is not in the reserved namespace",
                registered.name()
            );
            assert_eq!(
                super::capability(registered.name()).map(LocalCapability::name),
                Some(registered.name())
            );
            assert!(!registered.operations().is_empty());
        }
        assert!(super::capability("local.absent").is_none());
        assert!(super::capability("stripe").is_none());

        // A capability outside the namespace does not build, and neither does
        // one that declares an operation twice.
        assert!(
            LocalCapability::declare("document", "1.0.0")
                .operation(probe_operation())
                .build()
                .is_err()
        );
        assert!(
            LocalCapability::declare("local.probe", "1.0.0")
                .operation(probe_operation())
                .operation(probe_operation())
                .build()
                .is_err()
        );
        assert!(
            LocalCapability::declare("local.probe", "1.0.0")
                .build()
                .is_err(),
            "a capability with no operation is nothing"
        );
    }

    /// The one gate metadata validation asks: an unknown operation and a
    /// non-executable one are both refused from the declaration.
    #[test]
    fn only_a_declared_operation_is_admitted() {
        let capability = LocalCapability::declare("local.probe", "1.0.0")
            .operation(probe_operation())
            .build()
            .expect("a complete declaration is valid");
        assert_eq!(
            capability
                .admit_operation("probe.render")
                .map(LocalOperation::id),
            Ok("probe.render")
        );
        assert_eq!(
            capability
                .admit_operation("probe.absent")
                .map(LocalOperation::id),
            Err(crate::sdk::OperationRejection::Undeclared)
        );
    }

    /// The five bounds the probe operation runs inside, small enough that each
    /// boundary is reachable in a test.
    fn test_bounds() -> LocalBounds {
        LocalBounds::declare(Duration::from_millis(200), 128, 64, 256, "items", 8)
            .expect("a complete bound declaration is valid")
    }

    /// A capability whose whole job is to reach each of its five bounds: it
    /// counts the units its input declares, reserves the working memory its
    /// input declares, spins for as long as its input declares, and produces an
    /// artifact of exactly the size its input declares.
    fn probe_operation() -> LocalOperation {
        fn units(input: &JsonValue) -> u64 {
            input.get("units").and_then(JsonValue::as_u64).unwrap_or(1)
        }

        fn run(
            invocation: &LocalInvocation<'_>,
        ) -> Result<LocalProduct, crate::sdk::ConnectorFailure> {
            let input = invocation.input();
            let reserve = input
                .get("reserve")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0) as usize;
            invocation.reserve(reserve)?;

            let spin = input
                .get("spin_ms")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let until = std::time::Instant::now() + Duration::from_millis(spin);
            while std::time::Instant::now() < until {
                invocation.checkpoint()?;
                std::thread::yield_now();
            }

            let size = input.get("output").and_then(JsonValue::as_u64).unwrap_or(1) as usize;
            Ok(LocalProduct::Artifact {
                artifact: LocalArtifact::new(
                    "public.probe.artifact",
                    "app",
                    "probe.bin",
                    "application/octet-stream",
                    vec![b'x'; size],
                )?,
                metadata: json!({ "bytes": size }),
            })
        }

        LocalOperation::declare("probe.render", "1.0.0")
            .effect(Effect::pure(
                DeterminismEvidence::double_render(
                    json!({ "units": 1, "output": 1 }),
                    "the output is the input's declared size, and nothing else",
                )
                .expect("a probe and a statement are evidence"),
            ))
            .bounds(test_bounds())
            .units(units)
            .run(run)
            .build()
            .expect("a deterministic probe operation registers")
    }

    /// An input whose canonical form is exactly `bytes` long.
    fn padded_input(bytes: usize) -> JsonValue {
        let fixed = canonical_bytes(&json!({ "units": 1, "output": 1, "pad": "" })).len();
        json!({ "units": 1, "output": 1, "pad": "x".repeat(bytes - fixed) })
    }
}
