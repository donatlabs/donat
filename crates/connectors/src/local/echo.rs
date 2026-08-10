//! `local.echo` — the smallest capability that is a real one.
//!
//! It exists so the local path has something to run: one operation that
//! produces a typed value and one that produces bytes, which are the two shapes
//! every later capability takes (spec 019's email is the first, its PDF the
//! second). It is deliberately trivial — the interesting content of specs
//! 019–022 is their backends, not their wiring — but it is not a stub: it
//! declares its five bounds, proves its determinism at registration, and hands
//! its bytes to the attachment store like any other producer.

use std::time::Duration;

use serde_json::{Value as JsonValue, json};

use crate::local::bounds::LocalBounds;
use crate::local::capability::{
    LocalArtifact, LocalCapability, LocalInvocation, LocalOperation, LocalProduct,
};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare("local.echo", "1.0.0")
        .operation(value_echo())
        .operation(text_artifact())
        .build()
        .expect("the echo capability declaration is static and complete")
}

/// `value.echo`: the input's `value`, and the number of characters in it.
fn value_echo() -> LocalOperation {
    LocalOperation::declare("value.echo", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({ "value": "donat" }),
                "the output is the declared input value and its character count; \
                 no clock, no random seed, no environment, no locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(
            LocalBounds::declare(
                Duration::from_secs(1),
                8_192,
                8_192,
                8_192,
                "characters",
                4_096,
            )
            .expect("the echo bounds are static and complete"),
        )
        .units(|input| text(input, "value").map(count_characters).unwrap_or(0))
        .run(run_value_echo)
        .build()
        .expect("value.echo is deterministic")
}

fn run_value_echo(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let value = text(invocation.input(), "value")
        .ok_or_else(|| contract("local capability input requires a string `value`"))?;
    // The copy this operation makes is charged, because the working-memory
    // ceiling is only a ceiling if the implementation charges against it.
    invocation.reserve(value.len())?;
    invocation.checkpoint()?;
    Ok(LocalProduct::Value(json!({
        "value": value,
        "characters": count_characters(value),
    })))
}

/// `text.artifact`: the input's `text`, repeated, stored as a text file.
fn text_artifact() -> LocalOperation {
    LocalOperation::declare("text.artifact", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "echo.txt",
                    "text": "donat\n",
                    "copies": 2
                }),
                "the output is the declared text repeated the declared number of times; \
                 no clock, no random seed, no environment, no locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(
            LocalBounds::declare(Duration::from_secs(2), 8_192, 65_536, 131_072, "copies", 64)
                .expect("the echo artifact bounds are static and complete"),
        )
        .units(|input| input.get("copies").and_then(JsonValue::as_u64).unwrap_or(1))
        .run(run_text_artifact)
        .build()
        .expect("text.artifact is deterministic")
}

fn run_text_artifact(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let attachment = text(input, "attachment").ok_or_else(|| {
        contract("local capability input requires the `attachment` its file belongs to")
    })?;
    let claim_role = text(input, "claim_role").ok_or_else(|| {
        contract("local capability input requires the `claim_role` that will bind its file")
    })?;
    let file_name = text(input, "file_name").unwrap_or("echo.txt");
    let source =
        text(input, "text").ok_or_else(|| contract("local capability input requires `text`"))?;
    let copies = input.get("copies").and_then(JsonValue::as_u64).unwrap_or(1);

    let size = source
        .len()
        .checked_mul(copies as usize)
        .ok_or_else(|| contract("local capability input requires a bounded repeat count"))?;
    invocation.reserve(size)?;

    let mut bytes = Vec::with_capacity(size);
    for _ in 0..copies {
        invocation.checkpoint()?;
        bytes.extend_from_slice(source.as_bytes());
    }

    Ok(LocalProduct::Artifact {
        artifact: LocalArtifact::new(attachment, claim_role, file_name, "text/plain", bytes)?
            .claimed_by_session(text(input, "claim_session_key"))?,
        metadata: json!({ "characters": count_characters(source) * copies }),
    })
}

fn text<'a>(input: &'a JsonValue, field: &str) -> Option<&'a str> {
    input.get(field).and_then(JsonValue::as_str)
}

fn count_characters(value: &str) -> u64 {
    value.chars().count() as u64
}

/// An input that does not satisfy the operation's contract is a `validation`
/// failure, exactly as an over-limit input is: the same input will fail again.
fn contract(message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Validation,
        "local_input_contract",
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::capability::StopSignal;
    use crate::local::context::LocalContext;

    #[test]
    fn value_echo_returns_the_value_and_counts_its_characters() {
        let capability = capability();
        let operation = capability
            .admit_operation("value.echo")
            .expect("value.echo is declared and executable");
        let product = operation
            .execute(
                &json!({ "value": "héllo" }),
                LocalContext::builtin(),
                None,
                &StopSignal::new(),
            )
            .expect("a declared input renders");
        assert_eq!(
            product,
            LocalProduct::Value(json!({ "value": "héllo", "characters": 5 })),
            "characters are counted, not bytes"
        );
        let failure = operation
            .execute(
                &json!({ "other": 1 }),
                LocalContext::builtin(),
                None,
                &StopSignal::new(),
            )
            .expect_err("an input outside the contract is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_input_contract");
    }

    /// The bytes half: what comes back is an artifact bound to a file column,
    /// with its typed metadata beside it — never the bytes in a JSON value.
    #[test]
    fn text_artifact_produces_bytes_bound_to_a_file_column() {
        let capability = capability();
        let operation = capability
            .admit_operation("text.artifact")
            .expect("text.artifact is declared and executable");
        let product = operation
            .execute(
                &json!({
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "receipt.txt",
                    "text": "ab",
                    "copies": 3
                }),
                LocalContext::builtin(),
                None,
                &StopSignal::new(),
            )
            .expect("a declared input renders");
        let LocalProduct::Artifact { artifact, metadata } = product else {
            panic!("text.artifact produces bytes, not a value");
        };
        assert_eq!(artifact.bytes(), b"ababab");
        assert_eq!(artifact.attachment(), "public.document.file");
        assert_eq!(artifact.claim_role(), "app");
        assert_eq!(artifact.file_name(), "receipt.txt");
        assert_eq!(artifact.media_type(), "text/plain");
        assert_eq!(metadata, json!({ "characters": 6 }));
        assert_eq!(
            artifact.claim_session_key(),
            None,
            "an activity that names no session identity produces a row with none"
        );
    }

    /// The identity half of the claim, which travels the way the role does.
    ///
    /// A pending upload is claimed on `session_role` *and* `session_key`, so a
    /// file produced for a role whose sessions carry an identity has to name
    /// that identity or the write can never bind it.
    #[test]
    fn a_produced_file_names_the_session_that_will_claim_it() {
        let capability = capability();
        let operation = capability
            .admit_operation("text.artifact")
            .expect("text.artifact is declared and executable");
        let render = |session: JsonValue| {
            operation.execute(
                &json!({
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "receipt.txt",
                    "text": "ab",
                    "claim_session_key": session
                }),
                LocalContext::builtin(),
                None,
                &StopSignal::new(),
            )
        };
        let LocalProduct::Artifact { artifact, .. } =
            render(json!("u-1")).expect("a declared session identity renders")
        else {
            panic!("text.artifact produces bytes, not a value");
        };
        assert_eq!(artifact.claim_session_key(), Some("u-1"));

        // It reaches a row and a comparison, so it is a plain value or it is
        // nothing: a blank one would silently mean "no identity".
        for refused in [json!(""), json!(" u-1"), json!("u\n1")] {
            let failure = render(refused.clone())
                .expect_err("a session identity that is not a plain value is refused");
            assert_eq!(
                failure.code(),
                "local_artifact_invalid",
                "{refused} must not become a session key"
            );
        }
    }
}
