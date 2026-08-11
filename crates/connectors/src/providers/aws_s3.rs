//! Amazon S3 — objects and buckets on one configured bucket.
//!
//! Written against Amazon's own published REST API reference. Two things about
//! the shape of this module follow from that documentation rather than from
//! taste.
//!
//! The endpoint is the **path-style** regional endpoint,
//! `https://s3.<region-code>.amazonaws.com/<bucket-name>/<key-name>`, which the
//! S3 user guide describes as one of the two supported forms: "Currently,
//! Amazon S3 supports both virtual-hosted–style and path-style URL access in
//! all AWS Regions." It is the form this SDK can express, because a
//! virtual-hosted host carries *two* deploy-time variables (the bucket and the
//! Region) while `OriginSpec::TemplatedHost` declares exactly one; and it is
//! the form `bucket.list` needs, because `ListBuckets` is a request to the
//! Region's own endpoint rather than to a bucket. The same page records that
//! "path-style URLs will be discontinued in the future" — when they are, this
//! connector needs a two-variable templated host, which is an SDK change.
//!
//! S3 answers in XML with metadata in response headers, while the SDK's
//! declared output pointers read JSON. The declarations here therefore fix the
//! *request* — origin, method, path, query, bounds, effect class — and this
//! module transcribes each documented response into the operation's output
//! object. The error map still does the classifying: an S3 `<Error><Code>` is
//! lifted into the one JSON field the closed map reads, so nothing about
//! classification moves out of the SDK.
//!
//! Two operations here rest on SDK capabilities added for them, and both are
//! worth naming because they are the places this connector is least ordinary:
//!
//! * `object.head` — `HeadObject` is an HTTP `HEAD` ("The `HEAD` operation
//!   retrieves metadata from an object without returning the object itself"),
//!   which the SDK's closed method set admits as a read: a `HEAD` retrieves and
//!   returns exactly as a `GET` does, so it needs no provider assertion to be
//!   `ReadOnly`.
//! * `object.copy` — `CopyObject` requires the `x-amz-copy-source` header,
//!   whose *value* the SDK binds from a declared input slot while the header
//!   name stays in the declaration. The value is composed here from the
//!   configured bucket and a percent-encoded source key, so a copy can only
//!   ever read from this deployment's own bucket. AWS also documents that "A
//!   `200 OK` response can contain either a success or an error", so the
//!   declared success status is not the whole contract: [`S3Instance::decode`]
//!   reads the body of the 200 and routes an `<Error><Code>` through the error
//!   map instead of publishing it as an output.

use std::time::Duration;

use donat_value_contract::ValueScalar;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::providers::aws::{self, ConfigurationError};
use crate::sdk::connector::OperationRejection;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{OperationError, Required};
use crate::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, CredentialSpec, Effect, Operation, Origin,
    OriginSpec, PaginationBudget, RequestPlan,
};

pub const CONNECTOR_NAME: &str = "aws_s3";
pub const CONNECTOR_VERSION: &str = "1.0.0";
/// The AWS service code of the credential scope.
pub const SERVICE: &str = "s3";
const REQUEST_SHAPE_VERSION: &str = "1.0.0";
/// `https://s3.{{region-code}}.amazonaws.com/{{bucket-name}}/{{key-name}}`.
const HOST_TEMPLATE: &str = "s3.{region}.amazonaws.com";
/// The configuration key that fills the templated host.
pub const REGION_CONFIGURATION_KEY: &str = "region";

/// The header AWS documents for `CopyObject`: "Specifies the source object for
/// the copy operation. The source object can be up to 5 GB."
const COPY_SOURCE_HEADER: &str = "x-amz-copy-source";

/// The operation input slot that header's value is composed into. It is filled
/// by [`S3Instance::plan`] from the configured bucket and the caller's source
/// key, never by the caller directly — see [`aws::RESERVED_INPUT_NAMES`].
const COPY_SOURCE_INPUT: &str = "copy_source";

/// The one part of a copy source a caller chooses: the key inside the
/// configured bucket.
const SOURCE_KEY_INPUT: &str = "source_key";

/// The largest object body this connector will send or accept, unless a
/// deployment lowers it. It is a Donat bound, not an AWS one: AWS's own
/// `PutObject` ceiling is far larger, and a durable activity that moved
/// gigabytes through one step would not be a bounded activity.
pub const DEFAULT_MAX_OBJECT_BYTES: usize = 4 * 1024 * 1024;

/// Whether the configured bucket keeps object versions.
///
/// This is deploy-time configuration and it decides one effect class. AWS
/// documents that on a versioning-enabled bucket a keyless delete "creates a
/// delete marker over the current version of the object and returns its version
/// ID in the response", so a second identical `DELETE` leaves a *second* delete
/// marker — one more resource than the first send left. `object.delete` is
/// therefore admitted as executable only on an unversioned bucket, where AWS
/// documents that "you can specify the object's key in the `Delete` API
/// operations and Amazon S3 will permanently delete the object".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketVersioning {
    Unversioned,
    Versioned,
}

impl BucketVersioning {
    pub fn parse(value: &str) -> Result<Self, ConfigurationError> {
        match value {
            "unversioned" => Ok(Self::Unversioned),
            "versioned" => Ok(Self::Versioned),
            _ => Err(ConfigurationError::new(
                "bucket_versioning",
                "bucket_versioning is `unversioned` or `versioned`",
            )),
        }
    }
}

/// One deployment's S3 configuration. Every field here is deploy-time; none of
/// them is reachable from operation input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Configuration {
    region: String,
    bucket: String,
    versioning: BucketVersioning,
    max_object_bytes: usize,
}

impl S3Configuration {
    pub fn new(
        region: &str,
        bucket: &str,
        versioning: BucketVersioning,
    ) -> Result<Self, ConfigurationError> {
        aws::validate_region(region)?;
        validate_bucket_name(bucket)?;
        Ok(Self {
            region: region.to_owned(),
            bucket: bucket.to_owned(),
            versioning,
            max_object_bytes: DEFAULT_MAX_OBJECT_BYTES,
        })
    }

    /// Lower the object ceiling. It can only be lowered: the compiled default
    /// is the largest object this connector will ever move.
    pub fn with_max_object_bytes(mut self, bytes: usize) -> Result<Self, ConfigurationError> {
        if bytes == 0 || bytes > DEFAULT_MAX_OBJECT_BYTES {
            return Err(ConfigurationError::new(
                "max_object_bytes",
                "max_object_bytes is positive and at most the compiled object ceiling",
            ));
        }
        self.max_object_bytes = bytes;
        Ok(self)
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub const fn versioning(&self) -> BucketVersioning {
        self.versioning
    }

    pub const fn max_object_bytes(&self) -> usize {
        self.max_object_bytes
    }

    /// The deploy-time values the templated host resolves from.
    pub fn connector_configuration(&self) -> ConnectorConfiguration {
        ConnectorConfiguration::from_deployment([(REGION_CONFIGURATION_KEY, self.region.as_str())])
    }
}

/// A general purpose bucket name, as AWS's bucket naming rules spell it, with
/// dots refused: AWS records that "When you're using virtual-hosted–style
/// general purpose buckets with SSL, the SSL wildcard certificate matches only
/// buckets that do not contain dots", and a bucket this connector cannot later
/// address virtual-hosted-style is a bucket this connector should not accept.
fn validate_bucket_name(bucket: &str) -> Result<(), ConfigurationError> {
    let valid = (3..=63).contains(&bucket.len())
        && bucket.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !bucket.starts_with('-')
        && !bucket.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(ConfigurationError::new(
            "bucket",
            "a bucket name is 3 to 63 lowercase letters, digits, and hyphens",
        ))
    }
}

/// This module's static declaration (spec 010 §4).
///
/// Its operations carry the generic `{bucket}` path slot, because a static
/// declaration predates any deployment. A compiled [`S3Instance`] rebuilds the
/// same operations with the configured bucket written into the path, so no
/// deployment ever renders a request whose bucket came from anywhere but its
/// own configuration.
pub fn connector() -> &'static Connector {
    static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        Connector::declare(CONNECTOR_NAME, CONNECTOR_VERSION)
            .origin(
                OriginSpec::templated_host("https", HOST_TEMPLATE, None)
                    .expect("the S3 regional endpoint template is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::aws_sigv4(SERVICE).expect("s3 is a static service code"),
            ))
            .operations(operations(None).expect("the S3 declaration is valid"))
            .build()
            .expect("the S3 declaration is valid")
    });
    &CONNECTOR
}

fn path(bucket: Option<&str>, suffix: &str) -> String {
    match bucket {
        Some(bucket) => format!("/{bucket}{suffix}"),
        None => format!("/{{bucket}}{suffix}"),
    }
}

/// Declare the bucket slot when the declaration has no configured bucket yet.
fn with_bucket_slot(
    builder: crate::sdk::OperationBuilder,
    bucket: Option<&str>,
) -> crate::sdk::OperationBuilder {
    match bucket {
        Some(_) => builder,
        // The slot exists only before a deployment configured its bucket, and
        // it is filled from that configuration when one does — never from
        // input, which `aws::RESERVED_INPUT_NAMES` refuses outright.
        None => builder
            .untyped_path_param("bucket")
            .supplied_input("bucket"),
    }
}

fn operations(bucket: Option<&str>) -> Result<Vec<Operation>, OperationError> {
    Ok(vec![
        // GET /{bucket}/{key} — "If the action is successful, the service sends
        // back an HTTP 200 response."
        with_bucket_slot(
            Operation::get("object.get", &path(bucket, "/{key}"))
                .version(REQUEST_SHAPE_VERSION)
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                // The object arrives as bytes and its metadata as response
                // headers, so this contract is composed by `decode` rather than
                // read from JSON pointers.
                .declared_output("body_base64", ValueScalar::String, Required::Yes)
                .declared_output("content_length", ValueScalar::Int64, Required::Yes)
                .declared_output("etag", ValueScalar::String, Required::Yes)
                .declared_output("content_type", ValueScalar::String, Required::No)
                .effect(Effect::read_only()),
            bucket,
        )
        .build()?,
        // HEAD /{bucket}/{key} — "The HEAD operation retrieves metadata from an
        // object without returning the object itself", and "If the action is
        // successful, the service sends back an HTTP 200 response" whose
        // metadata is entirely in response headers. A `HEAD` retrieves and
        // returns, so it is read-only by its method exactly as a `GET` is.
        with_bucket_slot(
            Operation::head("object.head", &path(bucket, "/{key}"))
                .version(REQUEST_SHAPE_VERSION)
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .declared_output("etag", ValueScalar::String, Required::Yes)
                .declared_output("content_length", ValueScalar::Int64, Required::No)
                .declared_output("content_type", ValueScalar::String, Required::No)
                .declared_output("last_modified", ValueScalar::String, Required::No)
                .effect(Effect::read_only()),
            bucket,
        )
        .build()?,
        // PUT /{bucket}/{key}. `ProviderIdempotent::NaturalMethod`: AWS
        // documents that "Amazon S3 is a distributed system. If it receives
        // multiple write requests for the same object simultaneously, it
        // overwrites all but the last object written", and that "Amazon S3
        // never adds partial objects; if you receive a success response, Amazon
        // S3 added the entire object to the bucket". Two identical sends to one
        // fixed key therefore leave exactly one object with the sent content.
        with_bucket_slot(
            Operation::put("object.put", &path(bucket, "/{key}"))
                .version(REQUEST_SHAPE_VERSION)
                .path_param("key", ValueScalar::String)
                .processor_body("application/octet-stream")
                // The object's bytes are the operation's input even though no
                // template leaf renders them: `S3Instance::plan` reads them and
                // signs the payload hash over exactly what it sends.
                .declared_input("body", ValueScalar::String, Required::Yes)
                .success_statuses([StatusCode::OK])
                // "Returns the ETag of the new object", in a response header.
                .declared_output("etag", ValueScalar::String, Required::Yes)
                .effect(Effect::provider_idempotent_natural_method(
                    "Amazon S3 documents PutObject on a fixed key as overwriting: \"If it \
                     receives multiple write requests for the same object simultaneously, it \
                     overwrites all but the last object written\", and \"Amazon S3 never adds \
                     partial objects; if you receive a success response, Amazon S3 added the \
                     entire object to the bucket\"",
                )?),
            bucket,
        )
        .build()?,
        // PUT /{bucket}/{destination-key} with `x-amz-copy-source` — CopyObject.
        // The destination is the operation's own path key and the source is the
        // one header value this connector binds from input; both name the
        // configured bucket, because `S3Instance::plan` composes the header
        // value from the configured bucket and a percent-encoded source key.
        //
        // `ProviderIdempotent::NaturalMethod`: AWS documents the copy as "a
        // single atomic action" and documents a write to a fixed key as
        // overwriting — "If it receives multiple write requests for the same
        // object simultaneously, it overwrites all but the last object
        // written" — so two identical copies leave one object at the
        // destination key, exactly as two identical `PUT`s do.
        //
        // The declared success status is not the whole contract here: AWS
        // records that "A `200 OK` response can contain either a success or an
        // error. If you call the S3 API directly, make sure to design your
        // application to parse the content of the response and handle it
        // appropriately", so `S3Instance::decode` reads the body of the 200 and
        // routes an `<Error><Code>` through the error map rather than
        // publishing it as an output.
        with_bucket_slot(
            Operation::put("object.copy", &path(bucket, "/{key}"))
                .version(REQUEST_SHAPE_VERSION)
                .path_param("key", ValueScalar::String)
                .header_input(COPY_SOURCE_HEADER, COPY_SOURCE_INPUT, ValueScalar::String)
                // The header value is composed from the configured bucket and
                // the source key, so the key is the input and the composed
                // value is the connector's.
                .declared_input(SOURCE_KEY_INPUT, ValueScalar::String, Required::Yes)
                .supplied_input(COPY_SOURCE_INPUT)
                .success_statuses([StatusCode::OK])
                .declared_output("etag", ValueScalar::String, Required::Yes)
                .declared_output("last_modified", ValueScalar::String, Required::No)
                .effect(Effect::provider_idempotent_natural_method(
                    "Amazon S3 documents CopyObject as creating the copy \"in a single atomic \
                     action\", and documents a write to a fixed key as overwriting: \"If it \
                     receives multiple write requests for the same object simultaneously, it \
                     overwrites all but the last object written\". Two identical copies therefore \
                     leave one object at the destination key",
                )?),
            bucket,
        )
        .build()?,
        // DELETE /{bucket}/{key} — "If the action is successful, the service
        // sends back an HTTP 204 response." Executable only on an unversioned
        // bucket; see `BucketVersioning`.
        with_bucket_slot(
            Operation::delete("object.delete", &path(bucket, "/{key}"))
                .version(REQUEST_SHAPE_VERSION)
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::NO_CONTENT])
                .declared_output("deleted", ValueScalar::Boolean, Required::Yes)
                .effect(Effect::provider_idempotent_natural_method(
                    "Amazon S3 documents that on an unversioned bucket \"you can specify the \
                     object's key in the Delete API operations and Amazon S3 will permanently \
                     delete the object\", so a repeated DELETE of one key leaves the same one \
                     absent object",
                )?),
            bucket,
        )
        .build()?,
        // GET /{bucket}?list-type=2 — ListObjectsV2. "Sets the maximum number of
        // keys returned in the response. By default, the action returns up to
        // 1,000 key names. The response might contain fewer keys but will never
        // contain more."
        with_bucket_slot(
            Operation::get("object.list", &path(bucket, ""))
                .version(REQUEST_SHAPE_VERSION)
                .query_static("list-type", "2")
                .query_input("prefix", "prefix")
                .query_input("max-keys", "max_keys")
                // Both are defaulted by `S3Instance::plan` — the empty prefix
                // and AWS's own documented page — so a Process may omit them.
                .declared_input("prefix", ValueScalar::String, Required::No)
                .declared_input("max_keys", ValueScalar::Int64, Required::No)
                .success_statuses([StatusCode::OK])
                // ListObjectsV2 answers with an XML document, which the JSON
                // decoder cannot read; `decode` lifts these four out of it.
                .declared_output("keys", ValueScalar::Json, Required::Yes)
                .declared_output("is_truncated", ValueScalar::Boolean, Required::Yes)
                .declared_output("next_continuation_token", ValueScalar::String, Required::No)
                .declared_output("key_count", ValueScalar::Int64, Required::Yes)
                .effect(Effect::read_only()),
            bucket,
        )
        .build()?,
        // GET / — ListBuckets, on the Region's own endpoint.
        Operation::get("bucket.list", "/")
            .version(REQUEST_SHAPE_VERSION)
            .query_input("max-buckets", "max_buckets")
            .declared_input("max_buckets", ValueScalar::Int64, Required::No)
            .success_statuses([StatusCode::OK])
            .declared_output("buckets", ValueScalar::Json, Required::Yes)
            .effect(Effect::read_only())
            .build()?,
    ])
}

/// S3's documented failure codes, each reaching exactly one closed class.
///
/// The codes are the `Code` values of the REST error response, lifted out of
/// S3's XML envelope by [`S3Instance::classify`]. `SlowDown` — "Reduce your
/// request rate", published at `503 Slow Down` — is the throttling case, and it
/// reaches `http_429` rather than `http_5xx` even though its status is a 5xx.
/// `RequestTimeTooSkewed` — "The difference between the request time and the
/// server's time is too large" — is the clock-skew rejection, and it is an
/// authentication failure because that is what a signature outside AWS's
/// tolerance is.
pub fn error_map() -> ErrorMap {
    ErrorMap::builder(ConnectorErrorClass::Permanent)
        .code_pointer("/Code")
        .on_code("SlowDown", ConnectorErrorClass::Http429)
        .on_code("RequestTimeTooSkewed", ConnectorErrorClass::Authentication)
        .on_code("SignatureDoesNotMatch", ConnectorErrorClass::Authentication)
        .on_code("InvalidAccessKeyId", ConnectorErrorClass::Authentication)
        .on_code("AccessDenied", ConnectorErrorClass::Authentication)
        .on_code("EntityTooLarge", ConnectorErrorClass::Validation)
        .on_code("InvalidRequest", ConnectorErrorClass::Validation)
        .on_code("PreconditionFailed", ConnectorErrorClass::Validation)
        .on_code("NoSuchKey", ConnectorErrorClass::Permanent)
        .on_code("NoSuchBucket", ConnectorErrorClass::Permanent)
        .on_code("InternalError", ConnectorErrorClass::Http5xx)
        .on_code("ServiceUnavailable", ConnectorErrorClass::Http5xx)
        .on_status(429, ConnectorErrorClass::Http429)
        .on_statuses([401, 403], ConnectorErrorClass::Authentication)
        .on_statuses([400, 412], ConnectorErrorClass::Validation)
        .on_status(408, ConnectorErrorClass::Timeout)
        .on_statuses(500..=599, ConnectorErrorClass::Http5xx)
        .correlation_header("request_id", "x-amz-request-id")
        .build()
        .expect("the S3 error map is a valid declaration")
}

/// One compiled S3 connector instance: the resolved origin, the operations with
/// the configured bucket written into their paths, and this deployment's
/// bounds.
#[derive(Debug, Clone)]
pub struct S3Instance {
    configuration: S3Configuration,
    origin: Origin,
    operations: Vec<Operation>,
}

impl S3Instance {
    /// Compile one deployment's instance. Every refusal here happens at
    /// startup, before a listener opens.
    pub fn compile(configuration: &S3Configuration) -> Result<Self, ConfigurationError> {
        let origin = connector()
            .resolve_origin(&configuration.connector_configuration())
            .map_err(|_| {
                ConfigurationError::new("region", "the configured region is not an S3 endpoint")
            })?;
        Self::compile_against(configuration, origin)
    }

    /// The same compilation against an explicit origin, which is how a
    /// crate-local test points this connector at the SDK's provider stub. It is
    /// compiled only for tests; a deployment reaches [`S3Instance::compile`].
    #[cfg(any(test, feature = "testing"))]
    pub fn compile_for_stub(
        configuration: &S3Configuration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        Self::compile_against(configuration, origin)
    }

    fn compile_against(
        configuration: &S3Configuration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        let operations = operations(Some(configuration.bucket())).map_err(|_| {
            ConfigurationError::new(
                "bucket",
                "the configured bucket is not a valid path segment",
            )
        })?;
        Ok(Self {
            configuration: configuration.clone(),
            origin,
            operations,
        })
    }

    pub const fn configuration(&self) -> &S3Configuration {
        &self.configuration
    }

    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Every operation this compiled instance carries, in declaration order.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations
            .iter()
            .find(|operation| operation.id() == id)
    }

    /// The gate a deployment meets. It is the connector's own
    /// [`Connector::admit_operation`] plus the one rule that depends on this
    /// deployment's configuration: on a versioning-enabled bucket a repeated
    /// `DELETE` leaves a second delete marker, so `object.delete` is not
    /// repeat-safe there and is not executable.
    pub fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        let operation = self.operation(id).ok_or(OperationRejection::Undeclared)?;
        if !operation.is_executable() {
            return Err(OperationRejection::InventoryOnly);
        }
        if id == "object.delete" && self.configuration.versioning == BucketVersioning::Versioned {
            return Err(OperationRejection::InventoryOnly);
        }
        Ok(operation)
    }

    /// The budget one logical attempt spends. `object.list` returns one bounded
    /// page: AWS answers `ListObjectsV2` in XML, and every pagination plan in
    /// the SDK reads a JSON items pointer, so a continuation walk cannot be
    /// declared here. The page is bounded by `max-keys`, and a truncated page
    /// says so in its output rather than being silently cut short.
    pub fn list_budget(&self, time_to_live: Duration) -> PaginationBudget {
        PaginationBudget::new(
            1,
            1,
            MAX_LIST_KEYS as usize,
            self.configuration.max_object_bytes,
            time_to_live,
        )
    }

    /// Render one operation. Deploy-time material is merged in here, and an
    /// input that names any of it is refused.
    pub fn plan(
        &self,
        operation: &Operation,
        input: &JsonValue,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let mut rendered = aws::with_deploy_time_values(input, [])?;
        match operation.id() {
            "object.put" => {
                let body = object_body(&rendered)?;
                if body.len() > self.configuration.max_object_bytes {
                    // Before any request is made: the ceiling is a Donat bound,
                    // so it is applied to the value, not to a provider answer.
                    return Err(ConnectorFailure::validation(
                        "connector object body exceeds the configured ceiling",
                    ));
                }
                operation.plan_processor_request(&self.origin, &rendered, &HeaderMap::new(), body)
            }
            "object.copy" => {
                // The copy source is composed here, from the configured bucket
                // and a percent-encoded key, so the header value a caller
                // influences can only ever name an object in this deployment's
                // own bucket. `copy_source` itself is a reserved input name, so
                // a caller that tried to supply the whole value is refused
                // rather than silently overridden.
                let source = copy_source(&rendered, self.configuration.bucket())?;
                aws::defaulted(&mut rendered, COPY_SOURCE_INPUT, json!(source));
                operation.plan_request(&self.origin, &rendered)
            }
            "object.list" => {
                aws::defaulted(&mut rendered, "prefix", json!(""));
                aws::defaulted(&mut rendered, "max_keys", json!(MAX_LIST_KEYS));
                admit_list_page_size(&rendered)?;
                operation.plan_request(&self.origin, &rendered)
            }
            "bucket.list" => {
                aws::defaulted(&mut rendered, "max_buckets", json!(MAX_LIST_KEYS));
                operation.plan_request(&self.origin, &rendered)
            }
            _ => operation.plan_request(&self.origin, &rendered),
        }
    }

    /// Classify one failed response.
    ///
    /// S3 publishes its machine-readable code as the `<Code>` element of an XML
    /// error document, so it is lifted into the one JSON field the closed error
    /// map reads. No provider text crosses: only the code is transcribed, and
    /// the map answers with its own message.
    pub fn classify(&self, status: u16, headers: &HeaderMap, body: &[u8]) -> ConnectorFailure {
        let code = std::str::from_utf8(body)
            .ok()
            .and_then(|body| aws::xml_text(body, "Code"));
        let transcribed = match code {
            Some(code) => serde_json::to_vec(&json!({ "Code": code }))
                .expect("a transcribed error code serializes"),
            None => b"{}".to_vec(),
        };
        error_map().classify(status, headers, &transcribed)
    }

    /// The declared output of one operation, read from the documented response.
    ///
    /// A required field the provider did not send is a `validation` failure
    /// rather than a null, exactly as a declared output pointer would be.
    pub fn decode(
        &self,
        operation: &Operation,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        if body.len() > self.configuration.max_object_bytes {
            return Err(ConnectorFailure::validation(
                "connector provider response exceeds the configured ceiling",
            ));
        }
        if !operation.is_success(status) {
            return Err(self.classify(status, headers, body));
        }
        let text = || String::from_utf8_lossy(body).into_owned();
        match operation.id() {
            "object.get" => Ok(json!({
                "body_base64": base64_of(body),
                "content_length": body.len(),
                "etag": required_header(headers, "etag")?,
                "content_type": aws::header_text(headers, "content-type"),
            })),
            // The metadata AWS returns in the response headers of a `HEAD`, and
            // no object body at all.
            "object.head" => Ok(json!({
                "etag": required_header(headers, "etag")?,
                "content_length": aws::header_text(headers, "content-length")
                    .and_then(|value| value.parse::<i64>().ok()),
                "content_type": aws::header_text(headers, "content-type"),
                "last_modified": aws::header_text(headers, "last-modified"),
            })),
            // "Returns the ETag of the new object."
            "object.put" => Ok(json!({ "etag": required_header(headers, "etag")? })),
            // AWS: "A `200 OK` response can contain either a success or an
            // error." A declared success status is therefore not enough here,
            // and the body decides: an `<Error><Code>` goes through the same
            // closed error map a failing status would, and a body that is
            // neither the documented `CopyObjectResult` nor an error does not
            // satisfy the declared contract.
            "object.copy" => {
                let document = text();
                if aws::xml_text(&document, "Code").is_some() {
                    return Err(self.classify(status, headers, body));
                }
                Ok(json!({
                    "etag": required_element(&document, "ETag")?,
                    "last_modified": aws::xml_text(&document, "LastModified"),
                }))
            }
            "object.delete" => Ok(json!({ "deleted": true })),
            "object.list" => {
                let body = text();
                let keys = aws::xml_all(&body, "Key");
                if keys.len() > MAX_LIST_KEYS as usize {
                    return Err(ConnectorFailure::validation(
                        "connector provider returned more keys than the declared page",
                    ));
                }
                Ok(json!({
                    "keys": keys,
                    // "Set to `false` if all of the results were returned. Set
                    // to `true` if more keys are available to return."
                    "is_truncated": required_element(&body, "IsTruncated")? == "true",
                    "next_continuation_token": aws::xml_text(&body, "NextContinuationToken"),
                    "key_count": required_element(&body, "KeyCount")?
                        .parse::<i64>()
                        .map_err(|_| {
                            ConnectorFailure::validation(
                                "connector provider response did not satisfy the declared contract",
                            )
                        })?,
                }))
            }
            "bucket.list" => {
                let body = text();
                Ok(json!({ "buckets": aws::xml_all(&body, "Name") }))
            }
            _ => Err(ConnectorFailure::invariant(
                "connector operation is not compiled into this binary",
            )),
        }
    }
}

/// AWS: "By default, the action returns up to 1,000 key names."
const MAX_LIST_KEYS: i64 = 1_000;

fn admit_list_page_size(input: &JsonValue) -> Result<(), ConnectorFailure> {
    let requested = input.get("max_keys").and_then(JsonValue::as_i64);
    match requested {
        Some(value) if (1..=MAX_LIST_KEYS).contains(&value) => Ok(()),
        _ => Err(ConnectorFailure::validation(
            "connector list page size is outside the declared bounds",
        )),
    }
}

/// The `x-amz-copy-source` value of one copy: the configured bucket and the
/// caller's source key, percent-encoded.
///
/// AWS documents the value as `/<bucket>/<key>` and requires the key to be
/// URL-encoded. Encoding every byte outside `[A-Za-z0-9]` satisfies that and
/// does one thing more: a source key carrying `/`, `..`, or `?` cannot leave
/// the one bucket this deployment configured, because it cannot produce a
/// second path separator.
fn copy_source(input: &JsonValue, bucket: &str) -> Result<String, ConnectorFailure> {
    let Some(JsonValue::String(key)) = input.get(SOURCE_KEY_INPUT) else {
        return Err(ConnectorFailure::validation(
            "connector copy source key must be a string",
        ));
    };
    if key.is_empty() {
        return Err(ConnectorFailure::validation(
            "connector copy source key must not be empty",
        ));
    }
    Ok(format!(
        "/{bucket}/{}",
        utf8_percent_encode(key, NON_ALPHANUMERIC)
    ))
}

/// The object body an `object.put` sends: the exact bytes, which is also what
/// the signature's payload hash covers.
fn object_body(input: &JsonValue) -> Result<Vec<u8>, ConnectorFailure> {
    match input.get("body") {
        Some(JsonValue::String(body)) => Ok(body.as_bytes().to_vec()),
        _ => Err(ConnectorFailure::validation(
            "connector object body must be a string",
        )),
    }
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn required_header(headers: &HeaderMap, name: &str) -> Result<String, ConnectorFailure> {
    aws::header_text(headers, name).ok_or_else(|| {
        ConnectorFailure::validation(
            "connector provider response did not satisfy the declared contract",
        )
    })
}

fn required_element(body: &str, tag: &str) -> Result<String, ConnectorFailure> {
    aws::xml_text(body, tag).ok_or_else(|| {
        ConnectorFailure::validation(
            "connector provider response did not satisfy the declared contract",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::sdk::EffectClass;
    use crate::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};

    const ACCESS_KEY_ID: &str = "AKIDONATEXAMPLE";
    const BUCKET: &str = "donat-fixtures";

    fn configuration() -> S3Configuration {
        S3Configuration::new("eu-west-1", BUCKET, BucketVersioning::Unversioned)
            .expect("a static configuration is valid")
    }

    fn credential() -> crate::sdk::Credential {
        aws::credential(ACCESS_KEY_ID, SECRET_SENTINEL, "eu-west-1", None)
    }

    fn instance(stub: &ProviderStub) -> S3Instance {
        S3Instance::compile_for_stub(&configuration(), stub.origin())
            .expect("a static configuration compiles")
    }

    fn signed(instance: &S3Instance, id: &str, input: JsonValue) -> RequestPlan {
        let operation = instance.operation(id).expect("the operation is declared");
        let mut request = instance
            .plan(operation, &input)
            .expect("the request renders");
        AuthPlan::aws_sigv4(SERVICE)
            .expect("s3 is a static service code")
            .apply(&credential(), &mut request, None)
            .expect("the request signs");
        request
    }

    /// `aws_s3_request_shape`, `aws_s3_auth_is_applied`: the exact method,
    /// path, query, headers, and body of every declared operation, with the
    /// signature the SDK's AWS plan applied.
    #[tokio::test]
    async fn aws_s3_request_shape_and_auth_are_applied() {
        let stub = ProviderStub::start([
            Expectation::new("GET", "/donat-fixtures/report%2Ejson")
                .query("")
                .no_body()
                .respond_header("etag", "\"abc\"")
                .respond_bytes(200, "object bytes"),
            Expectation::new("PUT", "/donat-fixtures/report%2Ejson")
                .header("content-type", "application/octet-stream")
                .respond_header("etag", "\"abc\"")
                .respond_bytes(200, ""),
            Expectation::new("GET", "/donat-fixtures")
                .query("list-type=2&prefix=logs%2F&max-keys=10")
                .respond_bytes(
                    200,
                    "<ListBucketResult><KeyCount>1</KeyCount><IsTruncated>false</IsTruncated>\
                     <Contents><Key>logs/a.txt</Key></Contents></ListBucketResult>",
                ),
        ])
        .await;
        let instance = instance(&stub);

        for (id, input) in [
            ("object.get", json!({ "key": "report.json" })),
            (
                "object.put",
                json!({ "key": "report.json", "body": "object bytes" }),
            ),
            ("object.list", json!({ "prefix": "logs/", "max_keys": 10 })),
        ] {
            let request = signed(&instance, id, input);
            let authorization = request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .expect("every request is signed")
                .to_owned();
            assert!(
                authorization.starts_with(&format!("AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/"))
                    && authorization.contains("/eu-west-1/s3/aws4_request,"),
                "{id} carries a signature scoped to the configured region and service"
            );
            assert!(
                request.headers().contains_key("x-amz-content-sha256")
                    && request.headers().contains_key("x-amz-date"),
                "{id} carries the signed AWS headers"
            );
            // The sentinel secret reaches the derived key and nothing else.
            assert!(
                !format!("{:?} {authorization}", request.headers()).contains(SECRET_SENTINEL),
                "{id} must not carry the secret access key"
            );
            stub.send(request).await.expect("the stub answers");
        }
        stub.assert_satisfied();
    }

    /// `aws_s3_region_and_target_are_deploy_time`: input cannot change the
    /// region, endpoint, or bucket, and a hostile key stays one path segment.
    #[tokio::test]
    async fn aws_s3_region_and_target_are_deploy_time() {
        let stub = ProviderStub::start([]).await;
        let instance = instance(&stub);
        let operation = instance
            .operation("object.get")
            .expect("the operation is declared");

        for hostile in [
            json!({ "key": "a", "bucket": "attacker" }),
            json!({ "key": "a", "region": "us-east-1" }),
            json!({ "key": "a", "endpoint": "https://attacker.invalid" }),
            json!({ "key": "a", "host": "attacker.invalid" }),
        ] {
            let failure = instance
                .plan(operation, &hostile)
                .expect_err("deploy-time material is not input");
            assert_eq!(failure.class(), ConnectorErrorClass::Validation);
            assert_eq!(
                failure.code(),
                "connector_input_names_deploy_time_configuration"
            );
        }

        // A key that spells a whole other origin stays one encoded segment on
        // the configured bucket.
        let request = instance
            .plan(operation, &json!({ "key": "../../attacker.invalid/x" }))
            .expect("a hostile key renders");
        assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
        assert_eq!(
            request.url().path(),
            "/donat-fixtures/%2E%2E%2F%2E%2E%2Fattacker%2Einvalid%2Fx"
        );

        // And the production origin is the region's own endpoint, resolved from
        // configuration alone.
        assert_eq!(
            connector()
                .resolve_origin(&configuration().connector_configuration())
                .expect("a configured region resolves")
                .as_url()
                .as_str(),
            "https://s3.eu-west-1.amazonaws.com/"
        );
        assert!(
            S3Configuration::new(
                "eu-west-1",
                "attacker.invalid",
                BucketVersioning::Unversioned
            )
            .is_err(),
            "a bucket that is not a bucket name is refused at startup"
        );
        for region in ["", "EU-WEST-1", "eu-west-1.attacker.invalid"] {
            assert!(
                S3Configuration::new(region, BUCKET, BucketVersioning::Unversioned).is_err(),
                "region {region} is refused at startup"
            );
        }
    }

    /// `aws_s3_put_is_repeat_safe`: two identical `PUT`s leave one object and
    /// one recorded write.
    ///
    /// The store below is S3's documented semantics for a fixed key — a `PUT`
    /// replaces whatever is there — applied to the exact bytes the stub
    /// received. The stub asserts both requests were the same request; the
    /// store shows what those two requests left behind.
    #[tokio::test]
    async fn aws_s3_put_is_repeat_safe() {
        let put = || {
            Expectation::new("PUT", "/donat-fixtures/report%2Ejson")
                .header("content-type", "application/octet-stream")
                .respond_header("etag", "\"5eb63bbbe01eeed093cb22bb8f5acdc3\"")
                .respond_bytes(200, "")
        };
        let stub = ProviderStub::start([put(), put()]).await;
        let instance = instance(&stub);
        let operation = instance
            .operation("object.put")
            .expect("the operation is declared");

        let mut objects: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut recorded_writes = 0usize;
        for _ in 0..2 {
            let request = signed(
                &instance,
                "object.put",
                json!({ "key": "report.json", "body": "hello world" }),
            );
            let key = request.url().path().to_owned();
            let body = request.body().to_vec();
            let response = stub.send(request).await.expect("the stub answers");
            let output = instance
                .decode(
                    operation,
                    response.status.as_u16(),
                    response.headers(),
                    response.body(),
                )
                .expect("the declared output is satisfied");
            assert_eq!(
                output["etag"],
                json!("\"5eb63bbbe01eeed093cb22bb8f5acdc3\"")
            );
            // A PUT to a fixed key replaces the object at that key.
            if objects.insert(key, body).is_none() {
                recorded_writes += 1;
            }
        }

        assert_eq!(objects.len(), 1, "two identical PUTs leave one object");
        assert_eq!(recorded_writes, 1, "and one recorded write");
        assert_eq!(
            objects.values().next().map(Vec::as_slice),
            Some(b"hello world".as_slice()),
            "the object is the exact bytes that were sent"
        );
        stub.assert_satisfied();
    }

    /// `aws_s3_object_head`: `HeadObject` is an HTTP `HEAD`, it is a read, and
    /// its declared output is the metadata AWS returns in response headers.
    #[tokio::test]
    async fn aws_s3_object_head_reads_metadata_without_the_object() {
        let stub = ProviderStub::start([Expectation::new("HEAD", "/donat-fixtures/report%2Ejson")
            .query("")
            .no_body()
            .respond_header("etag", "\"abc\"")
            .respond_header("content-type", "application/json")
            .respond_bytes(200, "")])
        .await;
        let instance = instance(&stub);
        let head = instance
            .operation("object.head")
            .expect("the operation is declared");
        assert_eq!(head.method(), crate::sdk::HttpMethod::Head);
        assert_eq!(head.effect_class(), Some(EffectClass::ReadOnly));

        let request = signed(&instance, "object.head", json!({ "key": "report.json" }));
        assert_eq!(request.method(), reqwest::Method::HEAD);
        assert!(request.body().is_empty());
        stub.send(request).await.expect("the stub answers");
        stub.assert_satisfied();

        // The declared output is the documented metadata, and a response
        // without the required field is a validation failure rather than a null.
        let mut headers = HeaderMap::new();
        headers.insert("etag", "\"abc\"".parse().expect("a test header"));
        headers.insert("content-length", "12".parse().expect("a test header"));
        headers.insert("content-type", "text/plain".parse().expect("a test header"));
        headers.insert(
            "last-modified",
            "Mon, 10 Aug 2026 00:00:00 GMT"
                .parse()
                .expect("a test header"),
        );
        assert_eq!(
            instance
                .decode(head, 200, &headers, b"")
                .expect("the declared output is satisfied"),
            json!({
                "etag": "\"abc\"",
                "content_length": 12,
                "content_type": "text/plain",
                "last_modified": "Mon, 10 Aug 2026 00:00:00 GMT",
            })
        );
        assert_eq!(
            instance
                .decode(head, 200, &HeaderMap::new(), b"")
                .expect_err("a missing required field is not a null")
                .class(),
            ConnectorErrorClass::Validation
        );
        assert_eq!(
            instance
                .decode(head, 404, &HeaderMap::new(), b"")
                .expect_err("404 is not a declared success")
                .class(),
            ConnectorErrorClass::Permanent
        );
    }

    /// `aws_s3_object_copy`: the copy source is a declared header whose value
    /// binds from a declared input slot and can only ever name the configured
    /// bucket, the request is signed over that header, and AWS's documented
    /// "`200 OK` with an error inside" is a failure rather than a success.
    #[tokio::test]
    async fn aws_s3_object_copy_binds_its_source_and_reads_a_200_that_carries_an_error() {
        let copied = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CopyObjectResult>\
                      <ETag>\"5eb63bbbe01eeed093cb22bb8f5acdc3\"</ETag>\
                      <LastModified>2026-08-10T00:00:00.000Z</LastModified></CopyObjectResult>";
        let expectation = || {
            Expectation::new("PUT", "/donat-fixtures/archive%2Freport%2Ejson")
                .query("")
                .header("x-amz-copy-source", "/donat-fixtures/report%2Ejson")
                .respond_bytes(200, copied)
        };
        let stub = ProviderStub::start([expectation(), expectation()]).await;
        let instance = instance(&stub);
        let copy = instance
            .operation("object.copy")
            .expect("the operation is declared");

        // Two identical copies are one request against one destination
        // identity, which is what makes the `NaturalMethod` class admissible.
        let mut objects: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for _ in 0..2 {
            let request = signed(
                &instance,
                "object.copy",
                json!({ "key": "archive/report.json", "source_key": "report.json" }),
            );
            let authorization = request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .expect("the copy is signed")
                .to_owned();
            assert!(
                authorization.contains("x-amz-copy-source"),
                "the copy source is part of the signature: {authorization}"
            );
            let key = request.url().path().to_owned();
            let response = stub.send(request).await.expect("the stub answers");
            let output = instance
                .decode(
                    copy,
                    response.status.as_u16(),
                    response.headers(),
                    response.body(),
                )
                .expect("the declared output is satisfied");
            assert_eq!(
                output["etag"],
                json!("\"5eb63bbbe01eeed093cb22bb8f5acdc3\"")
            );
            assert_eq!(output["last_modified"], json!("2026-08-10T00:00:00.000Z"));
            objects.insert(key, response.body().to_vec());
        }
        assert_eq!(objects.len(), 1, "two identical copies leave one object");
        stub.assert_satisfied();

        // AWS: "A `200 OK` response can contain either a success or an error."
        // The declared success status alone is therefore not enough, and an
        // error inside a 200 reaches the closed error map like any other.
        for (body, expected) in [
            (
                "<Error><Code>InternalError</Code><Message>internal shard db-7</Message></Error>"
                    .as_bytes(),
                ConnectorErrorClass::Http5xx,
            ),
            (
                "<Error><Code>SlowDown</Code></Error>".as_bytes(),
                ConnectorErrorClass::Http429,
            ),
            // A 200 that is neither the documented result nor an error does not
            // satisfy the declared contract either.
            (b"", ConnectorErrorClass::Validation),
            (b"<CopyObjectResult/>", ConnectorErrorClass::Validation),
        ] {
            let failure = instance
                .decode(copy, 200, &HeaderMap::new(), body)
                .expect_err("a 200 that is not the documented success is a failure");
            assert_eq!(failure.class(), expected);
            let surface = format!(
                "{} {} {failure:?}",
                failure.safe_message(),
                failure.diagnostic()
            );
            assert!(!surface.contains("db-7"), "provider text must not leak");
        }

        // The copy source names the configured bucket and nothing else: the
        // input supplies a key, and a hostile key stays inside it.
        let request = instance
            .plan(
                copy,
                &json!({ "key": "b.json", "source_key": "../../attacker.invalid/x" }),
            )
            .expect("a hostile source key renders");
        assert_eq!(
            request
                .headers()
                .get("x-amz-copy-source")
                .and_then(|value| value.to_str().ok()),
            Some("/donat-fixtures/%2E%2E%2F%2E%2E%2Fattacker%2Einvalid%2Fx")
        );
        for hostile in [
            json!({ "key": "b.json", "copy_source": "/attacker/x" }),
            json!({ "key": "b.json", "source_key": "a", "bucket": "attacker" }),
        ] {
            let failure = instance
                .plan(copy, &hostile)
                .expect_err("deploy-time material is not input");
            assert_eq!(
                failure.code(),
                "connector_input_names_deploy_time_configuration",
                "{hostile}"
            );
        }
        for missing in [
            json!({ "key": "b.json" }),
            json!({ "key": "b.json", "source_key": "" }),
            json!({ "key": "b.json", "source_key": 7 }),
        ] {
            assert_eq!(
                instance
                    .plan(copy, &missing)
                    .expect_err("the source key is a declared, typed slot")
                    .class(),
                ConnectorErrorClass::Validation,
                "{missing}"
            );
        }
    }

    /// `aws_s3_effects_are_classified`: every operation carries a class, and
    /// `object.delete` is executable only where its repeat-safety is documented.
    #[test]
    fn aws_s3_effects_are_classified() {
        let declared = connector();
        assert_eq!(
            declared
                .operations()
                .iter()
                .map(|operation| (operation.id(), operation.effect_class()))
                .collect::<Vec<_>>(),
            vec![
                ("object.get", Some(EffectClass::ReadOnly)),
                ("object.head", Some(EffectClass::ReadOnly)),
                (
                    "object.put",
                    Some(EffectClass::ProviderIdempotentNaturalMethod)
                ),
                (
                    "object.copy",
                    Some(EffectClass::ProviderIdempotentNaturalMethod)
                ),
                (
                    "object.delete",
                    Some(EffectClass::ProviderIdempotentNaturalMethod)
                ),
                ("object.list", Some(EffectClass::ReadOnly)),
                ("bucket.list", Some(EffectClass::ReadOnly)),
            ]
        );
        assert_eq!(
            declared.admit_operation("object.restore"),
            Err(OperationRejection::Undeclared)
        );

        let unversioned = S3Instance::compile(&configuration()).expect("a configuration compiles");
        assert!(unversioned.admit_operation("object.delete").is_ok());
        assert!(unversioned.admit_operation("object.head").is_ok());
        assert!(unversioned.admit_operation("object.copy").is_ok());

        let versioned = S3Instance::compile(
            &S3Configuration::new("eu-west-1", BUCKET, BucketVersioning::Versioned)
                .expect("a static configuration is valid"),
        )
        .expect("a configuration compiles");
        assert_eq!(
            versioned.admit_operation("object.delete"),
            Err(OperationRejection::InventoryOnly),
            "a repeated DELETE on a versioned bucket leaves a second delete marker"
        );
        assert!(
            versioned.admit_operation("object.put").is_ok(),
            "a PUT overwrites on either kind of bucket"
        );
        assert!(
            versioned.admit_operation("object.copy").is_ok(),
            "a copy is a PUT to a fixed destination key on either kind of bucket"
        );
        assert!(BucketVersioning::parse("neither").is_err());
    }

    /// `aws_s3_error_map`: the documented codes each reach one class,
    /// throttling reaches `http_429`, and no provider text crosses.
    #[test]
    fn aws_s3_error_map() {
        let instance = S3Instance::compile(&configuration()).expect("a configuration compiles");
        let error = |code: &str| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code>\
                 <Message>internal shard db-7.internal rejected key {SECRET_SENTINEL}</Message>\
                 <RequestId>req_01H</RequestId></Error>"
            )
            .into_bytes()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-amz-request-id",
            "req_01H".parse().expect("a test header"),
        );

        for (status, code, expected) in [
            (503, "SlowDown", ConnectorErrorClass::Http429),
            (
                403,
                "RequestTimeTooSkewed",
                ConnectorErrorClass::Authentication,
            ),
            (
                403,
                "SignatureDoesNotMatch",
                ConnectorErrorClass::Authentication,
            ),
            (
                403,
                "InvalidAccessKeyId",
                ConnectorErrorClass::Authentication,
            ),
            (403, "AccessDenied", ConnectorErrorClass::Authentication),
            (400, "EntityTooLarge", ConnectorErrorClass::Validation),
            (412, "PreconditionFailed", ConnectorErrorClass::Validation),
            (404, "NoSuchKey", ConnectorErrorClass::Permanent),
            (404, "NoSuchBucket", ConnectorErrorClass::Permanent),
            (500, "InternalError", ConnectorErrorClass::Http5xx),
            (503, "ServiceUnavailable", ConnectorErrorClass::Http5xx),
            // Unmapped in both dimensions: the declared fallback answers.
            (418, "Teapot", ConnectorErrorClass::Permanent),
        ] {
            let failure = instance.classify(status, &headers, &error(code));
            assert_eq!(failure.class(), expected, "{code}");
            assert_eq!(failure.provider_status(), Some(status));
            let surface = format!(
                "{} {} {} {failure:?}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [SECRET_SENTINEL, "db-7.internal", "internal shard", code] {
                assert!(!surface.contains(leaked), "{code} leaked {leaked}");
            }
            assert_eq!(
                failure
                    .correlation_ids()
                    .get("request_id")
                    .map(String::as_str),
                Some("req_01H")
            );
        }

        // A body that is not S3's error envelope still reaches exactly one
        // class, by status.
        assert_eq!(
            instance
                .classify(429, &headers, b"<html>gateway</html>")
                .class(),
            ConnectorErrorClass::Http429
        );
        assert_eq!(
            instance.classify(500, &headers, b"").class(),
            ConnectorErrorClass::Http5xx
        );
    }

    /// `aws_s3_output_contract`: the declared output is complete and typed, and
    /// a missing required field is a `validation` failure rather than a null.
    #[tokio::test]
    async fn aws_s3_output_contract() {
        let stub = ProviderStub::start([]).await;
        let instance = instance(&stub);
        let mut headers = HeaderMap::new();
        headers.insert("etag", "\"abc\"".parse().expect("a test header"));
        headers.insert("content-type", "text/plain".parse().expect("a test header"));

        let get = instance.operation("object.get").expect("declared");
        assert_eq!(
            instance
                .decode(get, 200, &headers, b"object bytes")
                .expect("the declared output is satisfied"),
            json!({
                "body_base64": "b2JqZWN0IGJ5dGVz",
                "content_length": 12,
                "etag": "\"abc\"",
                "content_type": "text/plain",
            })
        );
        assert_eq!(
            instance
                .decode(get, 200, &HeaderMap::new(), b"object bytes")
                .expect_err("a missing required field is not a null")
                .class(),
            ConnectorErrorClass::Validation
        );

        let list = instance.operation("object.list").expect("declared");
        assert_eq!(
            instance
                .decode(
                    list,
                    200,
                    &HeaderMap::new(),
                    b"<ListBucketResult><KeyCount>2</KeyCount><IsTruncated>true</IsTruncated>\
                      <NextContinuationToken>tok</NextContinuationToken>\
                      <Contents><Key>a</Key></Contents><Contents><Key>b</Key></Contents>\
                      </ListBucketResult>",
                )
                .expect("the declared output is satisfied"),
            json!({
                "keys": ["a", "b"],
                "is_truncated": true,
                "next_continuation_token": "tok",
                "key_count": 2,
            })
        );
        assert_eq!(
            instance
                .decode(list, 200, &HeaderMap::new(), b"<ListBucketResult/>")
                .expect_err("a page with no KeyCount does not satisfy the contract")
                .class(),
            ConnectorErrorClass::Validation
        );

        let buckets = instance.operation("bucket.list").expect("declared");
        assert_eq!(
            instance
                .decode(
                    buckets,
                    200,
                    &HeaderMap::new(),
                    b"<ListAllMyBucketsResult><Buckets><Bucket><Name>one</Name></Bucket>\
                      <Bucket><Name>two</Name></Bucket></Buckets></ListAllMyBucketsResult>",
                )
                .expect("the declared output is satisfied"),
            json!({ "buckets": ["one", "two"] })
        );

        // An undeclared status never becomes a silent success.
        assert_eq!(
            instance
                .decode(
                    get,
                    404,
                    &HeaderMap::new(),
                    b"<Error><Code>NoSuchKey</Code></Error>"
                )
                .expect_err("404 is not a declared success")
                .class(),
            ConnectorErrorClass::Permanent
        );
    }

    /// `aws_s3_bounds`, `aws_s3_pagination_is_bounded`: an oversized body is
    /// refused before a request is made, an oversized response fails without
    /// partial output, and a list page is bounded at both ends.
    #[tokio::test]
    async fn aws_s3_bounds_and_pagination_are_bounded() {
        let stub = ProviderStub::start([]).await;
        let configuration = configuration()
            .with_max_object_bytes(64)
            .expect("a lowered ceiling is valid");
        let instance = S3Instance::compile_for_stub(&configuration, stub.origin())
            .expect("a static configuration compiles");
        let put = instance.operation("object.put").expect("declared");

        // The exact ceiling renders; one byte over is refused before any
        // request is made.
        assert!(
            instance
                .plan(put, &json!({ "key": "k", "body": "x".repeat(64) }))
                .is_ok()
        );
        let failure = instance
            .plan(put, &json!({ "key": "k", "body": "x".repeat(65) }))
            .expect_err("one byte over the configured ceiling is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        stub.assert_satisfied();

        // A response over the ceiling fails, and fails whole.
        let get = instance.operation("object.get").expect("declared");
        assert!(
            instance
                .decode(get, 200, &HeaderMap::new(), &[b'x'; 65])
                .is_err_and(|failure| failure.class() == ConnectorErrorClass::Validation)
        );

        // The list page is bounded at both ends, and the budget is one page.
        let list = instance.operation("object.list").expect("declared");
        for size in [json!(0), json!(1001), json!("many")] {
            assert!(
                instance
                    .plan(list, &json!({ "prefix": "", "max_keys": size }))
                    .is_err()
            );
        }
        assert!(
            instance
                .plan(list, &json!({ "prefix": "", "max_keys": 1000 }))
                .is_ok()
        );
        let budget = instance.list_budget(Duration::from_secs(5));
        assert_eq!(budget.max_calls(), 1);
        assert_eq!(budget.max_pages(), 1);
        assert!(budget.admit_call(1).is_err());
        assert!(
            instance
                .decode(
                    list,
                    200,
                    &HeaderMap::new(),
                    format!(
                        "<ListBucketResult><KeyCount>1</KeyCount><IsTruncated>false</IsTruncated>{}</ListBucketResult>",
                        "<Contents><Key>k</Key></Contents>".repeat(1001)
                    )
                    .as_bytes(),
                )
                .is_err(),
            "a page larger than the declared bound is never returned as a partial aggregate"
        );
    }
}
