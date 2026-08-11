//! Deploy-time material shared by the AWS connectors.
//!
//! Everything an AWS request needs beyond its operation input — the Region, the
//! account-scoped host component, the bucket, the queue URL, the sending
//! identity — is configured once, at deploy time, and validated here before a
//! listener opens. There is no constructor in this module that accepts an
//! operation input, a provider response, or a continuation.
//!
//! The three connectors that use it also share one rule, enforced by
//! [`refuse_deploy_time_inputs`]: an operation input that so much as *names* a
//! deploy-time slot is refused rather than ignored, so a caller cannot discover
//! that `region` is silently dropped and cannot ever be sure it was.

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::sdk::auth::field;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::operation::OperationError;
use crate::sdk::{Credential, Secret};

/// The input names an AWS connector never accepts, whichever operation is being
/// rendered: each one selects a destination or an identity, and all of them are
/// deploy-time configuration.
pub const RESERVED_INPUT_NAMES: &[&str] = &[
    "region",
    "endpoint",
    "host",
    "service",
    "url",
    "bucket",
    "queue_url",
    "queue",
    "account_id",
    // The composed `x-amz-copy-source` value of an S3 copy. It names a bucket
    // and is therefore a target: the connector composes it from the configured
    // bucket and the caller's source key, and a caller that supplied the whole
    // value would be choosing the bucket the copy reads from.
    "copy_source",
    "source_bucket",
    "access_key_id",
    "secret_access_key",
    "session_token",
    // A durable activity's stable key, not a caller's: an operation that binds
    // its idempotency key from input would let a caller replay or split a
    // deduplicated send.
    "idempotency_key",
    "message_deduplication_id",
];

/// A deploy-time configuration defect, reported at startup with the setting's
/// name and never its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationError {
    setting: &'static str,
    message: &'static str,
}

impl ConfigurationError {
    pub const fn new(setting: &'static str, message: &'static str) -> Self {
        Self { setting, message }
    }

    pub const fn setting(&self) -> &'static str {
        self.setting
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.setting, self.message)
    }
}

impl std::error::Error for ConfigurationError {}

impl From<ConfigurationError> for OperationError {
    fn from(_: ConfigurationError) -> Self {
        OperationError::new("an AWS connector configuration value is not valid")
    }
}

/// An AWS Region code, as the credential scope and the regional endpoint spell
/// it: "The Region code, service code, and termination string must use
/// lowercase characters" (AWS, *Create a signed AWS API request*).
///
/// It is also one DNS label of a regional endpoint such as
/// `s3.eu-west-1.amazonaws.com`, so a value with a dot, a slash, or an at sign
/// would be a different authority and is refused rather than escaped.
pub fn validate_region(region: &str) -> Result<(), ConfigurationError> {
    let valid = !region.is_empty()
        && region.len() <= 32
        && region.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !region.starts_with('-')
        && !region.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(ConfigurationError::new(
            "region",
            "an AWS region is one lowercase endpoint label, such as eu-west-1",
        ))
    }
}

/// A twelve-digit AWS account identifier, which is the account-scoped component
/// of an SQS queue URL.
pub fn validate_account_id(account_id: &str) -> Result<(), ConfigurationError> {
    if account_id.len() == 12
        && account_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        Ok(())
    } else {
        Err(ConfigurationError::new(
            "account_id",
            "an AWS account id is twelve digits",
        ))
    }
}

/// The resolved AWS credential the [`crate::sdk::AuthPlan::aws_sigv4`] plan
/// signs with.
///
/// The Region travels here rather than on the request because it is deploy-time
/// material: this is the `region_from_config` half of spec 010 §6's
/// `AwsSigV4 { service, region_from_config }`.
pub fn credential(
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    session_token: Option<&str>,
) -> Credential {
    let mut fields = vec![
        (field::AWS_ACCESS_KEY_ID, Secret::new(access_key_id)),
        (field::AWS_SECRET_ACCESS_KEY, Secret::new(secret_access_key)),
        (field::AWS_REGION, Secret::new(region)),
    ];
    if let Some(session_token) = session_token {
        fields.push((field::AWS_SESSION_TOKEN, Secret::new(session_token)));
    }
    Credential::from_fields(fields)
}

/// Refuse an operation input that names deploy-time material.
///
/// The refusal is the point: silently dropping a `region` a caller supplied
/// would leave the caller believing it had chosen one.
pub fn refuse_deploy_time_inputs(input: &JsonValue) -> Result<(), ConnectorFailure> {
    let Some(object) = input.as_object() else {
        return Err(ConnectorFailure::invariant(
            "a connector operation input is a JSON object",
        ));
    };
    if object
        .keys()
        .any(|name| RESERVED_INPUT_NAMES.contains(&name.as_str()))
    {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Validation,
            "connector_input_names_deploy_time_configuration",
            "connector operation input may not name a region, endpoint, or target",
        ));
    }
    Ok(())
}

/// One operation input with the deploy-time values this instance renders with.
pub fn with_deploy_time_values<'a>(
    input: &JsonValue,
    values: impl IntoIterator<Item = (&'a str, JsonValue)>,
) -> Result<JsonValue, ConnectorFailure> {
    refuse_deploy_time_inputs(input)?;
    let mut merged = input.as_object().cloned().unwrap_or_else(JsonMap::new);
    for (name, value) in values {
        merged.insert(name.to_owned(), value);
    }
    Ok(JsonValue::Object(merged))
}

/// Default one optional operation input, so a declared query slot the caller
/// left out renders its documented default instead of failing.
///
/// An explicit `null` is the same statement as an omission and reaches the same
/// default. The operation publishes these slots as nullable contract fields
/// ([[049-a-connector-publishes-the-declaration-it-was-admitted-on]]), so both
/// spellings are values the contract admits, and a null that fell through to
/// the renderer would fail on a field a Process was told it could send.
pub fn defaulted(input: &mut JsonValue, name: &str, default: JsonValue) {
    if let Some(object) = input.as_object_mut()
        && !object.get(name).is_some_and(|value| !value.is_null())
    {
        object.insert(name.to_owned(), default);
    }
}

/// The first `<tag>…</tag>` element's text, with XML's five predefined entities
/// resolved.
///
/// The AWS connectors need this because Amazon S3 answers in XML while the
/// SDK's declared output pointers read JSON. It is a scanner rather than a
/// parser on purpose: it resolves exactly the five predefined entities, has no
/// entity or DTD handling to exploit, and runs over a body the transport has
/// already bounded.
pub fn xml_text(body: &str, tag: &str) -> Option<String> {
    xml_all(body, tag).into_iter().next()
}

/// Every `<tag>…</tag>` element's text, in document order.
pub fn xml_all(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut remaining = body;
    while let Some(start) = remaining.find(&open) {
        let after = &remaining[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(unescape_xml(&after[..end]));
        remaining = &after[end + close.len()..];
    }
    found
}

fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A response header, as a `String`, when it is present and printable.
pub fn header_text(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_region_is_one_lowercase_endpoint_label() {
        for region in ["eu-west-1", "us-east-1", "ap-southeast-4"] {
            assert!(validate_region(region).is_ok(), "{region}");
        }
        for region in [
            "",
            "EU-WEST-1",
            "eu west 1",
            "eu-west-1.amazonaws.com",
            "eu-west-1/../us-east-1",
            "-eu-west-1",
            "eu-west-1-",
        ] {
            assert_eq!(
                validate_region(region)
                    .expect_err("a hostile region is refused at startup")
                    .setting(),
                "region",
                "{region}"
            );
        }
        assert!(validate_account_id("123456789012").is_ok());
        for account in ["", "12345678901", "1234567890123", "12345678901a"] {
            assert!(validate_account_id(account).is_err(), "{account}");
        }
    }

    #[test]
    fn an_input_that_names_deploy_time_material_is_refused_rather_than_ignored() {
        for hostile in [
            json!({ "region": "us-east-1" }),
            json!({ "bucket": "attacker" }),
            json!({ "endpoint": "https://attacker.invalid" }),
            json!({ "queue_url": "https://attacker.invalid/q" }),
            json!({ "key": "ok", "service": "iam" }),
        ] {
            let failure =
                refuse_deploy_time_inputs(&hostile).expect_err("deploy-time names are refused");
            assert_eq!(failure.class(), ConnectorErrorClass::Validation);
            assert_eq!(
                failure.code(),
                "connector_input_names_deploy_time_configuration"
            );
        }
        assert!(refuse_deploy_time_inputs(&json!({ "key": "report.json" })).is_ok());
        assert!(refuse_deploy_time_inputs(&json!("not an object")).is_err());

        assert_eq!(
            with_deploy_time_values(&json!({ "key": "a" }), [("bucket", json!("configured"))])
                .expect("deploy-time values merge"),
            json!({ "key": "a", "bucket": "configured" })
        );
    }

    /// A declared-optional input is published as a nullable contract field, so
    /// a Process may spell "I am not choosing one" as an explicit null exactly
    /// as it may by omission. Both must reach the documented default; a null
    /// that fell through would fail at render on a field the contract admits.
    #[test]
    fn an_optional_input_defaults_from_absence_and_from_an_explicit_null() {
        for supplied in [json!({}), json!({ "page_size": null })] {
            let mut input = supplied.clone();
            defaulted(&mut input, "page_size", json!(50));
            assert_eq!(
                input,
                json!({ "page_size": 50 }),
                "an optional input reaches its default from {supplied}"
            );
        }

        // A value the caller did choose is never overwritten.
        let mut chosen = json!({ "page_size": 10 });
        defaulted(&mut chosen, "page_size", json!(50));
        assert_eq!(chosen, json!({ "page_size": 10 }));
    }

    #[test]
    fn the_xml_scanner_reads_elements_and_resolves_the_predefined_entities() {
        let body = "<ListBucketResult><Contents><Key>a&amp;b</Key></Contents>\
                    <Contents><Key>c&lt;d</Key></Contents><IsTruncated>true</IsTruncated>\
                    </ListBucketResult>";
        assert_eq!(
            xml_all(body, "Key"),
            vec!["a&b".to_owned(), "c<d".to_owned()]
        );
        assert_eq!(xml_text(body, "IsTruncated").as_deref(), Some("true"));
        assert_eq!(xml_text(body, "NextContinuationToken"), None);
        // An unterminated element is not a value.
        assert_eq!(xml_text("<Key>unclosed", "Key"), None);
    }
}
