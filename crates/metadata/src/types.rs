//! Typed model of the Donat v2 metadata format (metadata directory version 3).
//!
//! Field names and shapes follow the v2 spec so that exported Donat metadata
//! (and the fixtures from `server/tests-py`) deserialize without translation.
//! Open-ended expressions (boolean filters, column presets) are kept as
//! `serde_json::Value` for now; they get a typed AST when the sqlgen
//! milestone needs to compile them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor},
    ser::SerializeMap,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Metadata {
    pub version: u32,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_roles: Vec<InheritedRole>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_collections: Vec<QueryCollection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<AllowlistEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_schemas: Vec<RemoteSchema>,
    /// Synchronous actions: custom GraphQL fields backed by an HTTP webhook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionEntry>,
    /// Custom GraphQL types referenced by action input/output.
    #[serde(default, skip_serializing_if = "CustomTypes::is_empty")]
    pub custom_types: CustomTypes,
    /// Recurring (cron) scheduled triggers: a webhook fired on a cron
    /// schedule with a static payload. Deploy-time configuration only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cron_triggers: Vec<CronTrigger>,
    /// REST endpoints exposing saved queries over templated URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rest_endpoints: Vec<RestEndpoint>,
    /// Declarative domain commands from the optional `commands.yaml` section.
    /// They are deploy-time declarations only; catalog compilation validates
    /// their references before any command can be exposed or executed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Command>,
    /// Declarative rules and decision tables from the single `rules.yaml`
    /// wrapper. They are deploy-time metadata; the rules crate parses their
    /// source expressions when metadata is validated.
    #[serde(default, skip_serializing_if = "RulesMetadata::is_empty")]
    pub rules: RulesMetadata,
    /// Compiled connector instances declared in the optional `connectors.yaml`
    /// section. They remain deploy-time metadata: values resolved from the
    /// environment are never written back into this structure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors: Vec<ConnectorInstance>,
    /// Durable process declarations from the optional `flows.yaml` section.
    /// Loading them is parse-only until the process compiler and journal
    /// runtime validate and execute the closed grammar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<Process>,
    /// Agent-facing MCP tools. Kept in a separate `mcp.yaml` so GraphQL
    /// metadata remains transport-neutral and MCP exposure is opt-in.
    #[serde(default, skip_serializing_if = "McpMetadata::is_empty")]
    pub mcp: McpMetadata,
    /// File attachments from the optional `storage.yaml` section: backends,
    /// the columns bound to them, and the collector's windows. An absent file
    /// leaves the whole feature — routes, root field, catalog, background
    /// task — out of the deployment.
    #[serde(default, skip_serializing_if = "StorageMetadata::is_empty")]
    pub storage: StorageMetadata,
}

/// One named deployment instance of a connector module compiled into the
/// serving binary. The metadata can select only an instance and a module name;
/// it cannot provide code, a package, or another runtime implementation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorInstance {
    pub name: String,
    pub module: String,
    #[serde(default)]
    pub config: ConnectorConfig,
    #[serde(default)]
    pub operations: Vec<ConnectorOperation>,
}

/// The currently supported, deploy-time connector configuration surface.
///
/// `http` and `stripe` are the only built-in module names. Module-specific
/// validation happens in the server crate so it can remain aligned with the
/// compiled module table; this type only accepts fields that are safe to keep
/// in metadata. A secret can appear only as [`SecretRef`].
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint_identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<ConnectorBaseUrl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ConnectorHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
}

/// A configured HTTP base URL. A literal URL is non-secret metadata; a value
/// read at server startup is represented by its environment variable name.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ConnectorBaseUrl {
    Literal(String),
    FromEnv(SecretRef),
}

/// A secret reference in deployment metadata. It intentionally retains only
/// the environment variable *name*; resolution occurs at startup and the
/// value is never serialized into [`Metadata`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub value_from_env: String,
}

/// A static connector header whose value is read from the named environment
/// variable. This spelling matches the connector YAML profile, while secret
/// fields such as `secret_key` use [`SecretRef`] directly.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorHeader {
    pub name: String,
    pub value_from_env: String,
}

/// An enabled, named connector operation. The common identity and worker-owned
/// capacity policy are kept beside a closed module operation profile. Runtime
/// input can fill only the explicit `{ input: name }` values inside that
/// profile; it can never select a raw transport request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConnectorOperation {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<ConnectorCapacity>,
    #[serde(flatten)]
    pub profile: ConnectorOperationProfile,
}

impl ConnectorOperation {
    pub fn capacity(&self) -> Option<&ConnectorCapacity> {
        self.capacity.as_ref()
    }

    pub fn http(&self) -> Option<&HttpConnectorOperation> {
        match &self.profile {
            ConnectorOperationProfile::Http(operation) => Some(operation),
            ConnectorOperationProfile::Undeclared(_) => None,
        }
    }
}

/// Closed module operation profiles. `Undeclared` preserves the metadata
/// shape accepted before a module implements its operation contract; compiled
/// registry admission rejects it for an HTTP connector.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ConnectorOperationProfile {
    Http(Box<HttpConnectorOperation>),
    Undeclared(UndeclaredConnectorOperation),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndeclaredConnectorOperation {}

/// The static request/response contract of one declarative HTTP operation.
/// There is deliberately no URL, authority, dynamic method, or dynamic header
/// key field in this type.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConnectorOperation {
    #[serde(default)]
    pub version: String,
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, ConnectorInputBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_contract: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ConnectorStaticHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_statuses: Vec<u16>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response: BTreeMap<String, ConnectorResponseBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<ConnectorEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<ConnectorBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_map: Option<ConnectorErrorMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<ConnectorRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<ConnectorRedaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_contract: Option<ConnectorSuccessContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<ConnectorIdempotency>,
    #[serde(
        default,
        skip_serializing_if = "ConnectorHttpErrorClassification::is_empty"
    )]
    pub error_classification: ConnectorHttpErrorClassification,
}

/// A named JSON input slot in an operation template.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorInputBinding {
    pub input: String,
}

/// A deployed static request header. Credentials remain on the instance
/// configuration path and are resolved from named environment variables.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorStaticHeader {
    pub name: String,
    pub value: String,
}

/// A declared response field selected from a provider JSON response.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorResponseBinding {
    pub json_pointer: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_items: Option<u32>,
}

/// Whether an operation is transport-only or carries provider side effects.
/// Side-effecting operations must retain the complete fixed provider
/// idempotency contract before the Process compiler may admit them.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ConnectorEffect {
    ReadOnly(ConnectorReadOnlyEffect),
    ProviderIdempotent {
        provider_idempotent: ProviderIdempotentEffect,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorReadOnlyEffect {
    ReadOnly,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdempotentEffect {
    pub side_effect_steps: Vec<ProviderIdempotentStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdempotentStep {
    pub step: String,
    pub fixed_binding: ProviderIdempotencyBinding,
    pub scope: String,
    pub minimum_retention_ms: u64,
    pub clock_safety_margin_ms: u64,
    pub evidence: ProviderIdempotencyEvidence,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdempotencyBinding {
    pub header: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdempotencyEvidence {
    pub source_record_id: String,
    pub fact_ids: Vec<String>,
}

/// Complete finite transport and canonical-output limits for one operation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorBounds {
    pub deadline_ms: u64,
    pub maximum_calls: u32,
    pub maximum_pages: u32,
    pub maximum_items: u32,
    pub maximum_aggregate_request_bytes: u64,
    pub maximum_aggregate_response_bytes: u64,
    pub maximum_output_canonical_bytes: u64,
    pub maximum_redirects: u32,
    pub maximum_json_depth: u32,
    pub maximum_json_nodes: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorErrorMap {
    pub rules: Vec<ConnectorErrorRule>,
    pub fallback: ConnectorError,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorErrorRule {
    pub statuses: Vec<u16>,
    #[serde(rename = "class")]
    pub class_: ConnectorErrorClass,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorError {
    #[serde(rename = "class")]
    pub class_: ConnectorErrorClass,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorErrorClass {
    Authentication,
    Transport,
    Timeout,
    #[serde(rename = "http_429")]
    Http429,
    #[serde(rename = "http_5xx")]
    Http5xx,
    Validation,
    Permanent,
    Invariant,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorRetry {
    pub maximum_attempts: u32,
    pub backoff: String,
    pub retry_on: Vec<ConnectorErrorClass>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorRedaction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_headers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_body: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_body: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ConnectorSuccessContract {
    Status {
        status: String,
    },
    Lookup {
        discriminator: String,
        cases: BTreeMap<String, ConnectorSuccessCase>,
        unproven_absence: ConnectorUnprovenAbsence,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSuccessCase {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exactly_one_non_empty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorUnprovenAbsence {
    pub error: ConnectorError,
}

/// A provider idempotency header selected by metadata. The header name is
/// static; its value is supplied only from the stable logical activity key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorIdempotency {
    pub header: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorHttpErrorClassification {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_5xx: Vec<u16>,
}

impl ConnectorHttpErrorClassification {
    pub fn is_empty(&self) -> bool {
        self.http_5xx.is_empty()
    }
}

/// Shared operation limits enforced by the future process worker, not a
/// per-process connector client.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorCapacity {
    pub max_in_flight: u32,
    pub rate_limit: ConnectorRateLimit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serialize_by: Option<ConnectorSerializeBy>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorRateLimit {
    pub permits: u32,
    pub per: String,
    pub burst: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSerializeBy {
    pub input: String,
}

/// A named, deploy-time domain operation. The metadata crate preserves the
/// complete declaration without validating source, catalog, rule, process, or
/// permission references; those checks need the compiled catalog.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<CommandPermission>,
    #[serde(default, deserialize_with = "deserialize_command_arguments")]
    pub arguments: Vec<CommandArgument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<CommandGuard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<CommandStep>,
    #[serde(default, skip_serializing_if = "CommandResult::is_empty")]
    pub result: CommandResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<CommandIdempotency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<CommandEffect>,
}

/// A classic explicit role allowed to invoke a command. This is an additional
/// gate; later validation still requires the role's table permissions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPermission {
    pub role: String,
}

/// An insertion-ordered result mapping. Command results are exposed in the
/// exact order declared in metadata, so a sorted map would change the public
/// command contract while loading YAML.
#[derive(Debug, Clone, Default)]
pub struct CommandResult {
    pub fields: Vec<CommandResultField>,
}

impl CommandResult {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&CommandResultValue> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| &field.value)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.fields.iter().map(|field| &field.name)
    }
}

/// One named entry in a [`CommandResult`].
#[derive(Debug, Clone)]
pub struct CommandResultField {
    pub name: String,
    pub value: CommandResultValue,
}

impl<'de> Deserialize<'de> for CommandResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CommandResultVisitor;

        impl<'de> Visitor<'de> for CommandResultVisitor {
            type Value = CommandResult;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a mapping of command result fields")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut fields = Vec::new();
                while let Some((name, value)) = map.next_entry()? {
                    fields.push(CommandResultField { name, value });
                }
                Ok(CommandResult { fields })
            }
        }

        deserializer.deserialize_map(CommandResultVisitor)
    }
}

impl Serialize for CommandResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for field in &self.fields {
            map.serialize_entry(&field.name, &field.value)?;
        }
        map.end()
    }
}

/// A command result may expose an ordinary scalar command reference, a
/// bounded projected row set, or an explicitly declared literal array. It is
/// separate from [`CommandValue`] because output aliases and bounds are
/// result-contract concerns rather than inputs to command operations.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandResultValue {
    ProjectedStep {
        step: String,
        project: BTreeMap<String, String>,
        maximum_items: u32,
    },
    Step {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum_items: Option<u32>,
    },
    Argument {
        arg: String,
    },
    Literal {
        literal: serde_json::Value,
        /// Optional explicit result scalar. Nullable literals (notably
        /// `null`) cannot be inferred and therefore require this annotation.
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
    },
    Rule {
        rule: String,
        #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
        bindings: BTreeMap<String, CommandValue>,
    },
    SessionVariable {
        session_variable: String,
    },
    CurrentColumn {
        current_column: String,
    },
    Array(Vec<serde_json::Value>),
}

/// A typed command argument. The canonical metadata form is an ordered list;
/// the accepted mapping shorthand normalizes to this list during loading.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandArgument {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn deserialize_command_arguments<'de, D>(deserializer: D) -> Result<Vec<CommandArgument>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Arguments {
        List(Vec<CommandArgument>),
        Mapping(BTreeMap<String, String>),
    }

    Ok(match Option::<Arguments>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(Arguments::List(arguments)) => arguments,
        Some(Arguments::Mapping(arguments)) => arguments
            .into_iter()
            .map(|(name, type_)| CommandArgument {
                name,
                type_,
                description: None,
            })
            .collect(),
    })
}

/// A named boolean rule evaluated before the command steps execute.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGuard {
    pub rule: String,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One named command operation. Exactly one operation key is retained by the
/// externally tagged enum; operation-specific safety checks remain deferred to
/// catalog compilation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandStep {
    pub name: String,
    #[serde(flatten)]
    pub operation: CommandStepOperation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandStepOperation {
    SelectOne {
        select_one: SelectOneCommandStep,
    },
    SelectMany {
        select_many: SelectManyCommandStep,
    },
    Insert {
        insert: InsertCommandStep,
    },
    InsertMany {
        insert_many: InsertManyCommandStep,
    },
    Update {
        update: UpdateCommandStep,
    },
    UpdateMany {
        update_many: UpdateManyCommandStep,
    },
    Delete {
        delete: DeleteCommandStep,
    },
    Aggregate {
        aggregate: AggregateCommandStep,
    },
    Assert {
        assert: AssertCommandStep,
    },
    Decision {
        decision: DecisionCommandStep,
    },
    DecisionMany {
        decision_many: DecisionManyCommandStep,
    },
    Project {
        project: ProjectCommandStep,
    },
    ProjectMany {
        project_many: ProjectManyCommandStep,
    },
    FixedRows {
        fixed_rows: FixedRowsCommandStep,
    },
    AllocateMany {
        allocate_many: AllocateManyCommandStep,
    },
    AssertWhen {
        assert_when: ConditionalAssertCommandStep,
    },
    UpdateWhen {
        update_when: ConditionalUpdateCommandStep,
    },
    InsertWhen {
        insert_when: ConditionalInsertCommandStep,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectOneCommandStep {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default = "default_true")]
    pub require_found: bool,
}

/// A bounded ordered row-set read. Catalog compilation later proves the
/// relation, predicate columns, source, and role permissions; this type keeps
/// the YAML grammar closed before that phase.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectManyCommandStep {
    pub table: QualifiedTable,
    #[serde(deserialize_with = "deserialize_non_empty_command_value_map")]
    pub by: BTreeMap<String, CommandValue>,
    #[serde(deserialize_with = "deserialize_non_empty_unique_columns")]
    pub order_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default)]
    pub require_non_empty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_rows: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InsertCommandStep {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InsertManyCommandStep {
    pub table: QualifiedTable,
    pub for_each: CommandValue,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default)]
    pub allow_empty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_items: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCommandStep {
    pub table: QualifiedTable,
    #[serde(rename = "where", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub predicate: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default = "default_true")]
    pub require_affected: bool,
}

/// A bounded update over one prior row-set or one explicitly bounded typed
/// argument list. The catalog-aware command validator owns primary-key,
/// row-set source, and `current_column` scope checks; metadata parsing only
/// retains the closed declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManyCommandStep {
    pub table: QualifiedTable,
    pub for_each: CommandValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_items: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_items: Option<u32>,
    pub by: BTreeMap<String, CommandValue>,
    pub set: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CommandRuleBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default)]
    pub require_each: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteCommandStep {
    pub table: QualifiedTable,
    #[serde(rename = "where", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub predicate: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default = "default_true")]
    pub require_affected: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssertCommandStep {
    pub rule: String,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionCommandStep {
    pub decision_table: String,
    pub input: BTreeMap<String, CommandValue>,
    pub returning: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionManyCommandStep {
    pub decision_table: String,
    pub from: CommandValue,
    pub input: BTreeMap<String, CommandValue>,
    pub returning: Vec<String>,
    pub order_by: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCommandStep {
    pub values: BTreeMap<String, CommandValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManyCommandStep {
    pub from: CommandValue,
    pub maximum_rows: u32,
    pub values: BTreeMap<String, CommandValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixedRowsCommandStep {
    pub maximum_rows: u32,
    pub rows: Vec<BTreeMap<String, CommandValue>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllocateManyCommandStep {
    pub from: CommandValue,
    pub request_id: CommandValue,
    pub group_key: Vec<String>,
    pub exact_quantity_columns: ExactQuantityColumns,
    pub allocation_id: AllocationIdStrategy,
    pub returning: AllocationReturning,
    pub group_order_by: Vec<String>,
    pub line_order_by: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExactQuantityColumns {
    pub requested: String,
    pub available: String,
    pub allocated: String,
    pub backordered: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationIdStrategy {
    Deterministic,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllocationReturning {
    pub groups: Vec<String>,
    pub lines: Vec<String>,
    pub backorders: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalAssertCommandStep {
    pub when: CommandCondition,
    pub rule: String,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalUpdateCommandStep {
    pub when: CommandCondition,
    pub table: QualifiedTable,
    #[serde(rename = "where", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub predicate: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default = "default_true")]
    pub require_affected: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalInsertCommandStep {
    pub when: CommandCondition,
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandCondition {
    ArgumentEquals {
        argument_equals: ArgumentEqualsCondition,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentEqualsCondition {
    pub argument: String,
    pub value: serde_json::Value,
}

/// A closed Rule invocation bound to a relational update check. It has no
/// message or executable expression surface; Rule lookup is deferred to the
/// catalog-aware command compiler.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRuleBinding {
    pub rule: String,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, CommandValue>,
}

/// One aggregation over a prior bounded row-set or one explicitly bounded
/// typed argument list. Its source and column semantics are validated after
/// metadata loading, against the command graph and catalog.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateCommandStep {
    pub from: CommandValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_items: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_items: Option<u32>,
    pub values: BTreeMap<String, CommandAggregate>,
}

/// The only aggregations available to a relational command batch. There is no
/// free-form expression, filter, grouping, or window declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandAggregate {
    Count {
        count: CountCommandAggregate,
    },
    Sum {
        sum: ColumnCommandAggregate,
    },
    Min {
        min: ColumnCommandAggregate,
    },
    Max {
        max: ColumnCommandAggregate,
    },
    CountDistinct {
        count_distinct: ColumnCommandAggregate,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CountCommandAggregate {
    Enabled(bool),
    Options(CountCommandAggregateOptions),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CountCommandAggregateOptions {}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnCommandAggregate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

fn deserialize_non_empty_command_value_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, CommandValue>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = BTreeMap::deserialize(deserializer)?;
    if values.is_empty() {
        return Err(serde::de::Error::custom(
            "must contain at least one equality binding",
        ));
    }
    Ok(values)
}

fn deserialize_non_empty_unique_columns<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let columns = Vec::<String>::deserialize(deserializer)?;
    if columns.is_empty() {
        return Err(serde::de::Error::custom("must contain at least one column"));
    }
    let mut seen = BTreeSet::new();
    if columns.iter().any(|column| !seen.insert(column)) {
        return Err(serde::de::Error::custom(
            "must not contain duplicate columns",
        ));
    }
    Ok(columns)
}

/// A closed, SQL-free reference used by command steps, results, guards, and
/// process effects. No variant can carry a SQL fragment or identifier template.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandValue {
    Argument {
        arg: String,
    },
    Item {
        item: String,
    },
    Step {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        where_nonzero: Option<String>,
    },
    Literal {
        literal: serde_json::Value,
        /// Optional scalar annotation for contexts such as `fixed_rows`
        /// where no destination column supplies a type.
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
    },
    Rule {
        rule: String,
        #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
        bindings: BTreeMap<String, CommandValue>,
    },
    SessionVariable {
        session_variable: String,
    },
    CurrentColumn {
        current_column: String,
    },
    DatabaseTime {
        database_time: String,
    },
}

/// Optional replay protection for a command. Its scope is deliberately typed
/// so a later validator can reject non-deterministic declarations.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandIdempotency {
    pub key: CommandIdempotencyKey,
    #[serde(default, skip_serializing_if = "CommandIdempotencyScopeSpec::is_empty")]
    pub scope: CommandIdempotencyScopeSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
}

/// An idempotency key is intentionally separate from an ordinary command
/// value: command objects use `{ arg: ... }`, while the canonical
/// idempotency surface uses `{ argument: ... }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandIdempotencyKey {
    Argument { argument: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandIdempotencyScope {
    Argument { argument: String },
    SessionVariable { session_variable: String },
    Step { step: String, column: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandIdempotencyScopeSpec {
    Command(CommandIdempotencyCommandScope),
    Values(Vec<CommandIdempotencyScope>),
}

impl CommandIdempotencyScopeSpec {
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Values(values) if values.is_empty())
    }
}

impl Default for CommandIdempotencyScopeSpec {
    fn default() -> Self {
        Self::Values(Vec::new())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandIdempotencyCommandScope {
    Command,
}

/// A durable hand-off requested by a command. Compilation pins its Process
/// contract; command execution writes the corresponding source-local outbox
/// row atomically with domain data and the command invocation journal.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CommandEffect {
    StartProcess { start_process: StartProcessEffect },
    SignalProcess { signal_process: SignalProcessEffect },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartProcessEffect {
    pub process: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_key: Option<CommandValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<CommandIdempotencyKey>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignalProcessEffect {
    pub process: String,
    pub signal: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub correlate: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payload: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<CommandIdempotencyKey>,
}

/// A source-local durable process definition. The metadata layer retains only
/// the finite executable grammar; reference, type, and transition validation
/// belongs to the Process compiler.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub name: String,
    pub kind: ProcessKind,
    pub version: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "ProcessLifecycle::is_active")]
    pub lifecycle: ProcessLifecycle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<ProcessPermission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ProcessOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<ProcessStart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<ProcessField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<ProcessField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<ProcessIdempotency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<ProcessSignal>,
    pub start_at: String,
    pub states: Vec<ProcessState>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    Process,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessLifecycle {
    #[default]
    Active,
    Retired,
}

impl ProcessLifecycle {
    fn is_active(&self) -> bool {
        *self == Self::Active
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPermission {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_variable: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessOwner {
    #[serde(rename = "type")]
    pub type_: String,
    pub capture: ProcessValue,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessStart {
    pub command: String,
    pub input: BTreeMap<String, ProcessCommandResultReference>,
    pub idempotency_key: ProcessCommandArgumentReference,
    pub process_key: ProcessCommandResultReference,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCommandResultReference {
    pub command_result: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCommandArgumentReference {
    pub command_argument: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessField {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdempotency {
    pub key: ProcessIdempotencyValue,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<ProcessIdempotencyValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessIdempotencyValue {
    Input { input: String },
    SessionVariable { session_variable: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSignal {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub correlation: BTreeMap<String, String>,
    pub payload: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessState {
    pub id: String,
    #[serde(flatten)]
    pub operation: ProcessStateOperation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessStateOperation {
    Command { command: ProcessCommandState },
    Request { request: ProcessRequestState },
    When { when: ProcessWhenState },
    Wait { wait: ProcessWaitState },
    ForEach { for_each: Box<ProcessForEachState> },
    Output { output: ProcessOutputState },
    Fail { fail: ProcessFailState },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCommandState {
    pub name: String,
    pub run_as: String,
    pub arguments: BTreeMap<String, ProcessValue>,
    pub next: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCommandActivity {
    pub name: String,
    pub run_as: String,
    pub arguments: BTreeMap<String, ProcessValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequestState {
    pub connector: String,
    pub operation: String,
    pub input: BTreeMap<String, ProcessValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<ProcessRequestIdempotencyKey>,
    pub timeout: ProcessTimeout,
    pub retry: ProcessRetry,
    pub next: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<ProcessErrorRoutes>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequestActivity {
    pub connector: String,
    pub operation: String,
    pub input: BTreeMap<String, ProcessValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<ProcessRequestIdempotencyKey>,
    pub timeout: ProcessTimeout,
    pub retry: ProcessRetry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<ProcessErrorRoutes>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequestIdempotencyKey {
    pub stable: ProcessStableActivityKey,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessStableActivityKey {
    pub run: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTimeout {
    pub schedule_to_start: String,
    pub start_to_close: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRetry {
    pub retry_on: Vec<ProcessErrorKind>,
    pub max_attempts: u32,
    pub initial_interval: String,
    pub max_interval: String,
    pub jitter: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessErrorKind {
    Authentication,
    Transport,
    Timeout,
    #[serde(rename = "http_429")]
    Http429,
    #[serde(rename = "http_5xx")]
    Http5xx,
    Validation,
    Permanent,
    Invariant,
    RetryExhausted,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessErrorRoutes {
    pub routes: Vec<ProcessErrorRoute>,
    pub fallback: ProcessErrorFallback,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessErrorRoute {
    pub kinds: Vec<ProcessErrorKind>,
    pub next: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessErrorFallback {
    pub next: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWhenState {
    pub cases: Vec<ProcessWhenCase>,
    pub default: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_table: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input: BTreeMap<String, ProcessValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWhenCase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, ProcessValue>,
    pub next: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessWaitState {
    Signal(ProcessSignalWait),
    Webhook(ProcessWebhookWait),
    Timer(ProcessTimerWait),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSignalWait {
    pub signal: String,
    pub role: String,
    pub verification: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub persist_before_match: bool,
    pub correlate: BTreeMap<String, ProcessValue>,
    pub deadline: ProcessDeadline,
    pub next: String,
    pub on_timeout: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWebhookWait {
    pub webhook: ProcessWebhookSubscription,
    pub deadline: ProcessDeadline,
    pub next: String,
    pub on_timeout: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWebhookSubscription {
    pub connector: String,
    pub trigger: String,
    pub correlate: BTreeMap<String, ProcessValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<ProcessWebhookGuard>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessWebhookGuard {
    pub rule: String,
    #[serde(rename = "with")]
    pub bindings: BTreeMap<String, ProcessWebhookGuardValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessWebhookGuardValue {
    Event { event: String },
    Process(ProcessValue),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTimerWait {
    pub timer: ProcessTimer,
    pub next: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ProcessDeadline {
    Value(ProcessValue),
    Duration(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessTimer {
    pub decision_table: String,
    #[serde(rename = "with")]
    pub bindings: BTreeMap<String, ProcessValue>,
    pub output: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessForEachState {
    Command {
        input: ProcessValue,
        item_key: String,
        max_items: u32,
        max_concurrency: u32,
        completion: String,
        #[serde(default, skip_serializing_if = "is_false")]
        preserve_input: bool,
        command: ProcessCommandActivity,
        next: String,
    },
    Request {
        input: ProcessValue,
        item_key: String,
        max_items: u32,
        max_concurrency: u32,
        completion: String,
        #[serde(default, skip_serializing_if = "is_false")]
        preserve_input: bool,
        // Boxed because a request activity is far larger than a command one,
        // and an unboxed variant would make every `ProcessForEachState` — most
        // of them commands — carry the request variant's footprint.
        request: Box<ProcessRequestActivity>,
        next: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessOutputState {
    pub values: BTreeMap<String, ProcessValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessFailState {
    pub code: String,
    pub message: String,
}

/// Closed recursive value references available to process states. Bounded
/// collection transforms carry their maxima in the declaration; no generic
/// expression, loop, or executable code can be embedded here.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessValue {
    Input {
        input: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        require_non_null: bool,
    },
    State {
        state: String,
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<Vec<String>>,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        require_non_null: bool,
    },
    Item {
        item: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
    },
    Literal {
        literal: serde_json::Value,
    },
    ActivityKey {
        activity_key: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
    },
    ActivityKeyForState {
        activity_key_for_state: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        as_: Option<String>,
    },
    Run {
        run: String,
    },
    WorkflowTime {
        workflow_time: String,
    },
    SessionVariable {
        session_variable: String,
    },
    BoundedConcat {
        bounded_concat: ProcessBoundedConcat,
    },
    BoundedFlatten {
        bounded_flatten: Box<ProcessBoundedFlatten>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessBoundedConcat {
    pub inputs: Vec<ProcessValue>,
    pub maximum_lists: u32,
    pub maximum_items: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessBoundedFlatten {
    pub from: Box<ProcessValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<BTreeMap<String, String>>,
    pub maximum_lists: u32,
    pub maximum_items: u32,
}

/// The single `rules.yaml` metadata wrapper. Rules and decision tables share
/// one deploy-time section so they cannot be loaded or mutated independently.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RulesMetadata {
    /// Finite named types used by the CEL profile. They stay in the same
    /// deploy-time wrapper as rules and decision tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<RuleTypeDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RuleDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_tables: Vec<DecisionTableDefinition>,
}

impl RulesMetadata {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.rules.is_empty() && self.decision_tables.is_empty()
    }
}

/// One finite named object, enum, or bounded opaque JSON declaration from
/// `rules.yaml`. Validation that exactly one body is present and that object
/// references form an acyclic graph belongs to deploy-time catalog
/// compilation, where metadata paths are known.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleTypeDeclaration {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<BTreeMap<String, String>>,
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_json: Option<RuleOpaqueJsonDeclaration>,
}

/// A JSON value that can be passed through the typed Rule boundary but cannot
/// be inspected through the expression grammar.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleOpaqueJsonDeclaration {
    pub maximum_bytes: u32,
    pub maximum_depth: u32,
    pub maximum_nodes: u32,
}

/// A named CEL-profile expression. The metadata crate intentionally preserves
/// parameter/result type declarations and source text as strings; parsing,
/// typing, and source locations are owned by `donat-rules`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
    pub result: String,
    pub expression: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A named, ordered decision table. It is metadata, not a database relation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecisionTableDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    pub output: BTreeMap<String, String>,
    pub hit_policy: String,
    #[serde(default)]
    pub rows: Vec<DecisionRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_cases: Vec<DecisionTableTestCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A single ordered decision-table row. Conditions remain expression source
/// strings until the rule catalog validates them against the declared inputs.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecisionRow {
    pub id: String,
    pub when: BTreeMap<String, String>,
    pub output: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A deploy-time assertion over one decision-table input and its expected
/// complete output. The rule catalog executes these before publishing metadata.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecisionTableTestCase {
    pub name: String,
    pub input: serde_json::Value,
    pub expect: DecisionTableExpectation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecisionTableExpectation {
    pub output: serde_json::Value,
    pub matched_row_id: String,
}

/// Metadata loaded from the optional top-level `mcp.yaml` file.
///
/// This is deliberately a presentation/policy layer: a tool can only invoke
/// a saved GraphQL query or an existing typed action. It never grants data
/// permissions and it never introduces an MCP-only execution path.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpMetadata {
    /// Whether `mcp.yaml` was present when metadata was loaded. This is kept
    /// separate from its contents because an empty file is still an explicit
    /// deny-all publication policy, not a request for legacy CRUD tools.
    #[serde(skip)]
    configured: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<McpTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_tools: Vec<McpTableTool>,
    #[serde(default, skip_serializing_if = "McpResources::is_empty")]
    pub resources: McpResources,
}

impl McpMetadata {
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// Mark this value as originating from a present `mcp.yaml` file.
    /// Programmatic metadata writers must call this before serialization when
    /// they intentionally need an empty explicit publication allowlist.
    pub fn mark_configured(&mut self) {
        self.configured = true;
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.table_tools.is_empty() && self.resources.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub source: McpToolSource,
    /// Explicit MCP policy. Empty means no role is granted MCP exposure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// Human-facing descriptions keyed by GraphQL variable name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpToolSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_query: Option<McpSavedQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpSavedQuery {
    pub collection: String,
    pub query: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTableTool {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub operations: Vec<McpTableOperation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTableOperation {
    pub operation: McpTableOperationKind,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTableOperationKind {
    Query,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpResources {
    #[serde(default)]
    pub schema: McpSchemaResource,
}

impl McpResources {
    pub fn is_empty(&self) -> bool {
        !self.schema.enabled
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpSchemaResource {
    #[serde(default)]
    pub enabled: bool,
}

/// A custom GraphQL field (query or mutation) resolved by calling an HTTP
/// handler (webhook), with input/output shaped by [`CustomTypes`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionEntry {
    pub name: String,
    pub definition: ActionDefinition,
    /// Roles allowed to call the action. Empty = available to every role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<ActionPermission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionDefinition {
    /// `synchronous` (default) or `asynchronous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `query` or `mutation` (default mutation).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub action_type: Option<String>,
    // `arguments: null` (no args) appears in exported metadata, so tolerate an
    // explicit null as "empty", not just an absent key.
    #[serde(
        default,
        deserialize_with = "null_as_empty_vec",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub arguments: Vec<ArgumentDefinition>,
    /// GraphQL type reference for the result, e.g. `UserId` or `[UserId]`.
    #[serde(default = "default_action_output_type")]
    pub output_type: String,
    /// Webhook URL ({{ENV}} templates allowed).
    ///
    /// Absent means the action is resolved **in-process** by a function the
    /// embedding host registered under the action's name. Which host is
    /// serving decides whether that is satisfiable, so neither can accept the
    /// declaration silently: `donat-server` has no way to call an in-process
    /// function and refuses such an action at boot, and an embedded host
    /// refuses one whose function nobody registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<String>,
    #[serde(default)]
    pub forward_client_headers: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ActionHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// Deserialize a list that may be written as an explicit `null` (meaning
/// "none"), as Donat's exported action metadata sometimes does.
fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn default_action_output_type() -> String {
    "jsonb".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArgumentDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionHeader {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_from_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionPermission {
    pub role: String,
}

/// Whether an Action is visible to one already-authenticated classic role.
///
/// Action permissions deliberately use direct role membership: inherited-role
/// expansion is not part of the legacy Action contract. An empty list is
/// public only *within* the explicit-role GraphQL surface; request
/// authentication still has to supply a role before this helper is called.
pub fn action_visible_to_role(action: &ActionEntry, role: &str) -> bool {
    action.permissions.is_empty()
        || action
            .permissions
            .iter()
            .any(|permission| permission.role == role)
}

/// A recurring scheduled trigger: the engine POSTs `payload` to `webhook`
/// on the cron `schedule`. Field names match Donat's `CronTriggerMetadata`
/// so exported metadata loads without translation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CronTrigger {
    pub name: String,
    /// Webhook URL ({{ENV}} templates allowed).
    pub webhook: String,
    /// Standard 5-field cron expression, evaluated in UTC.
    pub schedule: String,
    /// Static JSON body sent to the webhook (under the envelope's `payload`).
    /// Donat tolerates an absent or explicitly null payload; both mean "no
    /// payload" — we normalize to JSON null here and emit `{}`-or-null at
    /// delivery time.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Whether the trigger is exported in metadata. Default true; accepted
    /// for round-trip fidelity (it does not change delivery behavior).
    #[serde(default = "default_true")]
    pub include_in_metadata: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_conf: Option<CronRetryConf>,
    /// Custom headers sent with the webhook request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ActionHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Retry/timeout policy for scheduled triggers (Donat `RetryConfST`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CronRetryConf {
    #[serde(default)]
    pub num_retries: u32,
    #[serde(default = "default_retry_interval_seconds")]
    pub retry_interval_seconds: u64,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_tolerance_seconds")]
    pub tolerance_seconds: u64,
}

impl Default for CronRetryConf {
    fn default() -> Self {
        CronRetryConf {
            num_retries: 0,
            retry_interval_seconds: default_retry_interval_seconds(),
            timeout_seconds: default_timeout_seconds(),
            tolerance_seconds: default_tolerance_seconds(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !value
}
fn default_retry_interval_seconds() -> u64 {
    10
}
fn default_timeout_seconds() -> u64 {
    60
}
fn default_tolerance_seconds() -> u64 {
    21600
}

/// The action type system: input objects, output objects (which may relate to
/// tracked tables), custom scalars, and enums.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CustomTypes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_objects: Vec<InputObjectType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scalars: Vec<ScalarType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumType>,
}

impl CustomTypes {
    pub fn is_empty(&self) -> bool {
        self.input_objects.is_empty()
            && self.objects.is_empty()
            && self.scalars.is_empty()
            && self.enums.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InputObjectType {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<CustomTypeField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectType {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<CustomTypeField>,
    /// Relationships from this output object to tracked tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<CustomTypeRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomTypeField {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomTypeRelationship {
    pub name: String,
    /// `object` or `array`.
    #[serde(rename = "type")]
    pub type_: String,
    pub remote_table: QualifiedTable,
    /// Output-object field -> remote-table column.
    pub field_mapping: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScalarType {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnumType {
    pub name: String,
    pub values: Vec<EnumValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnumValue {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSchema {
    pub name: String,
    pub definition: RemoteSchemaDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<RemoteSchemaPermission>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSchemaDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_from_env: Option<String>,
    #[serde(default)]
    pub forward_client_headers: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customization: Option<RemoteSchemaCustomization>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSchemaCustomization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_fields_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_names: Option<NameCustomization>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_names: Vec<FieldNameCustomization>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NameCustomization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FieldNameCustomization {
    pub parent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSchemaPermission {
    pub role: String,
    pub definition: RemoteSchemaPermissionDefinition,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSchemaPermissionDefinition {
    pub schema: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueryCollection {
    pub name: String,
    pub definition: QueryCollectionDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueryCollectionDefinition {
    #[serde(default)]
    pub queries: Vec<CollectionQuery>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CollectionQuery {
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AllowlistEntry {
    pub collection: String,
}

/// A REST endpoint that exposes a saved query (from a [`QueryCollection`])
/// over a templated URL. `:param` segments in `url` bind to path variables.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RestEndpoint {
    pub name: String,
    /// URL template; `:param` segments are path variables (e.g. `pet/:id`).
    pub url: String,
    /// HTTP methods this endpoint answers, e.g. `["GET"]` or `["POST", "PUT"]`.
    pub methods: Vec<String>,
    pub definition: RestEndpointDefinition,
    /// How a caller with no JWT proves itself, and what it runs as. Absent
    /// means the role comes from headers exactly as it does on /v1/graphql.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticate: Option<EndpointAuthentication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RestEndpointDefinition {
    pub query: RestEndpointQuery,
}

/// References a [`CollectionQuery`] by collection and query name.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RestEndpointQuery {
    pub collection_name: String,
    pub query_name: String,
}

/// How a caller proves who it is on an endpoint that no browser drives.
///
/// A provider posting a callback cannot present a JWT, and the alternatives —
/// the admin secret, or the unauthorized role — are respectively not a
/// permission and a public mutation. So the endpoint declares a credential it
/// can verify, and names the role a verified request runs as.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointAuthentication {
    #[serde(flatten)]
    pub credential: EndpointCredential,
    /// The role a verified request runs as. An ordinary declared role: it
    /// resolves through its own table permissions and escalates nothing.
    pub run_as: String,
    /// Session variables a verified request carries, beyond the role.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub session_variables: BTreeMap<String, String>,
    /// Refused before verification, so an oversized body is never hashed.
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Which payloads this endpoint acts on. A verified request matching none
    /// of them is acknowledged and does nothing — see the 204 rule. Empty
    /// means "everything the operation accepts".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept: Vec<PayloadPredicate>,
}

fn default_max_body_bytes() -> usize {
    65_536
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointCredential {
    /// A digest over the exact bytes, which is why verification has to happen
    /// before the body is parsed: re-serializing a parsed document produces
    /// different bytes and a valid signature fails.
    Signature(SignatureScheme),
    /// A constant compared in constant time. Replayable, and it does not
    /// survive a log leak — offered for senders that provide nothing better.
    SharedSecret(SharedSecretScheme),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureScheme {
    /// Header carrying the digest.
    pub header: String,
    pub algorithm: SignatureAlgorithm,
    #[serde(default)]
    pub encoding: SignatureEncoding,
    /// Stripped from the header value before decoding, e.g. `sha256=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// What goes into the digest. `{body}` is the exact request bytes and
    /// `{timestamp}` the value named by `timestamp`. A method with no body
    /// signs `{path}` and `{query}` instead — which is why this is a template
    /// rather than a flag.
    #[serde(default = "default_signed_payload")]
    pub signed_payload: String,
    /// Where the timestamp comes from, when the template uses one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<TimestampSource>,
    /// How far the timestamp may be from now. Absent means unchecked, which
    /// makes the signature replayable — set it whenever the sender provides a
    /// timestamp at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance_seconds: Option<u64>,
    pub secret: SecretRef,
}

fn default_signed_payload() -> String {
    "{body}".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    HmacSha256,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureEncoding {
    #[default]
    Hex,
    Base64,
}

/// Where a signed timestamp is read from. Some senders put it in a header of
/// its own; others fold it into the signature header as `t=…,v1=…`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TimestampSource {
    /// A header carrying the timestamp on its own.
    Header { header: String },
    /// A `key=value` pair inside the signature header itself.
    SignatureHeaderField { field: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSecretScheme {
    pub header: String,
    pub secret: SecretRef,
}

/// A guard on the parsed payload, evaluated only after verification.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadPredicate {
    pub json_pointer: String,
    pub equals: String,
}

/// An inherited role combines the permissions of its parents.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InheritedRole {
    pub role_name: String,
    pub role_set: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    pub configuration: SourceConfiguration,
    #[serde(default)]
    pub tables: Vec<TableEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionEntry>,
}

/// A tracked SQL function exposed as a root field.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FunctionEntry {
    pub function: QualifiedTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<FunctionConfiguration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<FunctionPermission>,
}

/// Explicit per-role exposure of a tracked function (used when function
/// permissions are not inferred).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FunctionPermission {
    pub role: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FunctionConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_argument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// "mutation" exposes the function on the mutation root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposed_as: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Postgres,
    Sqlite,
    Mysql,
    Clickhouse,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
    /// Unsupported source kinds can carry connector-specific configuration.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConnectionInfo {
    pub database_url: DatabaseUrl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_prepared_statements: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DatabaseUrl {
    Url(String),
    FromEnv { from_env: String },
}

/// `table: foo`, `table: public.foo`, or `table: { schema: public, name: foo }`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(untagged)]
pub enum QualifiedTable {
    Name(String),
    Qualified { schema: String, name: String },
    Parts(Vec<String>),
}

impl<'de> Deserialize<'de> for QualifiedTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawQualifiedTable {
            Name(String),
            Qualified { schema: String, name: String },
            Parts(Vec<String>),
        }

        match RawQualifiedTable::deserialize(deserializer)? {
            RawQualifiedTable::Name(name) => {
                if let Some((schema, table)) = name.split_once('.')
                    && (schema.is_empty() || table.is_empty() || table.contains('.'))
                {
                    return Err(serde::de::Error::custom(format!(
                        "invalid qualified table name '{name}': expected 'name' or 'schema.name'"
                    )));
                }
                Ok(Self::Name(name))
            }
            RawQualifiedTable::Qualified { schema, name } => Ok(Self::Qualified { schema, name }),
            RawQualifiedTable::Parts(parts) => Ok(Self::Parts(parts)),
        }
    }
}

impl QualifiedTable {
    pub fn schema(&self) -> &str {
        match self {
            QualifiedTable::Name(name) => name
                .split_once('.')
                .map(|(schema, _)| schema)
                .unwrap_or("public"),
            QualifiedTable::Qualified { schema, .. } => schema,
            QualifiedTable::Parts(parts) => parts.first().map(String::as_str).unwrap_or("public"),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            QualifiedTable::Name(name) => {
                name.split_once('.').map(|(_, table)| table).unwrap_or(name)
            }
            QualifiedTable::Qualified { name, .. } => name,
            QualifiedTable::Parts(parts) => parts.last().map(String::as_str).unwrap_or(""),
        }
    }
}

impl fmt::Display for QualifiedTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.schema(), self.name())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TableEntry {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<TableConfiguration>,
    #[serde(default)]
    pub is_enum: bool,
    #[serde(default)]
    pub object_relationships: Vec<ObjectRelationship>,
    #[serde(default)]
    pub array_relationships: Vec<ArrayRelationship>,
    #[serde(default)]
    pub computed_fields: Vec<ComputedField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_relationships: Vec<RemoteRelationship>,
    #[serde(default)]
    pub insert_permissions: Vec<PermissionEntry<InsertPermission>>,
    #[serde(default)]
    pub select_permissions: Vec<PermissionEntry<SelectPermission>>,
    #[serde(default)]
    pub update_permissions: Vec<PermissionEntry<UpdatePermission>>,
    #[serde(default)]
    pub delete_permissions: Vec<PermissionEntry<DeletePermission>>,
    /// Permissions used only while executing a closed declarative Command.
    /// They never expose ordinary GraphQL/REST/MCP CRUD roots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_insert_permissions: Vec<PermissionEntry<InsertPermission>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_select_permissions: Vec<PermissionEntry<SelectPermission>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_update_permissions: Vec<PermissionEntry<UpdatePermission>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_delete_permissions: Vec<PermissionEntry<DeletePermission>>,
    /// Webhooks fired on row insert/update/delete (Donat event triggers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_triggers: Vec<EventTrigger>,
    /// Columns of this table that hold a file reference. Declared here, beside
    /// the table's permissions, because an attachment is a property of the
    /// table — `storage.yaml` carries only the deployment-wide backends,
    /// signing, and collector windows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// A table event trigger: a webhook called when rows change. Field names
/// match Donat's directory-format `EventTriggerConf` so exported metadata
/// loads without translation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventTrigger {
    pub name: String,
    pub definition: EventTriggerDefinition,
    /// Webhook URL ({{ENV}} templates allowed). Exactly one of `webhook` /
    /// `webhook_from_env` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_from_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_conf: Option<EventRetryConf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ActionHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Which operations fire the trigger, and which columns each carries.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EventTriggerDefinition {
    /// Allow manually-invoked events (via the metadata API in Donat; accepted
    /// for round-trip fidelity).
    #[serde(default)]
    pub enable_manual: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert: Option<OperationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<OperationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<OperationSpec>,
}

/// Per-operation spec: which columns are delivered (and, for update, which
/// columns trigger the event). `columns` is `"*"` or a list.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperationSpec {
    #[serde(default)]
    pub columns: Columns,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Columns>,
}

/// Retry/timeout policy for event triggers (Donat `RetryConf`). Note the
/// field names differ from cron's `RetryConfST` (`interval_sec` /
/// `timeout_sec` vs `retry_interval_seconds` / `timeout_seconds`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventRetryConf {
    #[serde(default)]
    pub num_retries: u32,
    #[serde(default = "default_interval_sec")]
    pub interval_sec: u64,
    #[serde(default = "default_event_timeout_sec")]
    pub timeout_sec: u64,
}

impl Default for EventRetryConf {
    fn default() -> Self {
        EventRetryConf {
            num_retries: 0,
            interval_sec: default_interval_sec(),
            timeout_sec: default_event_timeout_sec(),
        }
    }
}

fn default_interval_sec() -> u64 {
    10
}
fn default_event_timeout_sec() -> u64 {
    60
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TableConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_root_fields: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_column_names: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub column_config: BTreeMap<String, ColumnConfig>,
}

/// Per-column presentation metadata (Donat v2 `column_config.<column>`).
///
/// Only `custom_name` and `comment` carry meaning to this engine; the
/// `comment` is surfaced as a column's GraphQL-introspection `description`
/// and in the MCP `describe_table` tool. Any other keys Hasura/Donat might
/// emit are preserved in `extra` so metadata round-trips losslessly.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ColumnConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Unknown keys, kept for lossless round-trip.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectRelationship {
    pub name: String,
    pub using: ObjRelUsing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjRelUsing {
    /// Column(s) on this table holding the foreign key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_key_constraint_on: Option<ObjRelFkColumns>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_configuration: Option<ManualConfiguration>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ObjRelFkColumns {
    Single(String),
    Multiple(Vec<String>),
    Remote(ArrRelFkConstraint),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArrayRelationship {
    pub name: String,
    pub using: ArrRelUsing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArrRelUsing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_key_constraint_on: Option<ArrRelFkConstraint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_configuration: Option<ManualConfiguration>,
}

/// Foreign key on the *remote* table pointing back at this one.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArrRelFkConstraint {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManualConfiguration {
    pub remote_table: QualifiedTable,
    pub column_mapping: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insertion_order: Option<String>,
}

/// A field joined to a remote schema: per-row arguments from columns.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteRelationship {
    pub name: String,
    #[serde(default)]
    pub donat_fields: Vec<String>,
    #[serde(default)]
    pub remote_schema: String,
    /// { <remote root field>: { arguments: { arg: "$column" | literal } } }
    #[serde(default)]
    pub remote_field: serde_json::Value,
}

/// A computed field: a function over the table row, exposed as a field.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComputedField {
    pub name: String,
    pub definition: ComputedFieldDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComputedFieldDefinition {
    pub function: QualifiedTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_argument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_argument: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionEntry<T> {
    pub role: String,
    pub permission: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Boolean expression over rows (`{ author_id: { _eq: X-Donat-User-Id } }`).
/// Kept untyped until the sqlgen milestone.
pub type BoolExp = serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SelectPermission {
    pub columns: Columns,
    #[serde(default)]
    pub filter: BoolExp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default)]
    pub allow_aggregations: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computed_fields: Vec<String>,
}

/// One entry of a write permission's `validate` list.
///
/// `check` is a predicate over the session and the row and answers whether the
/// role may write it. A validator answers a different question — whether the
/// value itself is acceptable — over the row as written, and carries its own
/// message. Exactly one of `expression` and `not_null` is present; which one,
/// and what the referenced columns must be, is settled against the catalogue
/// during deploy-time compilation rather than here.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionValidator {
    /// Rule-profile source, type checked against the table's columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    /// A column that must not be null in the written row. It also refines that
    /// column to its non-null type for the entries that follow, which is what
    /// lets a later expression compare it at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_null: Option<String>,
    /// Scopes an `expression` to rows where this column is present, and makes
    /// it non-null inside that expression.
    ///
    /// It exists because presence is declared here, never inferred: the rule
    /// profile has no flow-sensitive refinement, so an `is_null` arm written
    /// inside the expression would not make the other arm type check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_present: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InsertPermission {
    #[serde(default)]
    pub check: BoolExp,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, serde_json::Value>,
    /// Optional in older metadata; absent means all columns.
    #[serde(default)]
    pub columns: Columns,
    #[serde(default)]
    pub backend_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validate: Vec<PermissionValidator>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdatePermission {
    #[serde(default)]
    pub columns: Columns,
    #[serde(default)]
    pub filter: BoolExp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<BoolExp>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validate: Vec<PermissionValidator>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeletePermission {
    #[serde(default)]
    pub filter: BoolExp,
}

/// Column list: either an explicit list or `"*"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Columns {
    #[default]
    Star,
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for Columns {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            List(Vec<String>),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Str(s) if s == "*" => Ok(Columns::Star),
            Raw::Str(s) => Err(serde::de::Error::custom(format!(
                "expected \"*\" or a list of columns, got string {s:?}"
            ))),
            Raw::List(cols) => Ok(Columns::List(cols)),
        }
    }
}

impl Serialize for Columns {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Columns::Star => serializer.serialize_str("*"),
            Columns::List(cols) => cols.serialize(serializer),
        }
    }
}

// ---------------------------------------------------------------------------
// Storage: file attachments (spec 008)
// ---------------------------------------------------------------------------

/// The optional `storage.yaml` section: the deployment-wide half of file
/// attachments — where bytes go, how URLs are signed, and how often the
/// collector runs. Which column holds a file is declared on the table itself
/// ([`TableEntry::attachments`]).
///
/// Presence is what enables the feature. When the file is absent this value is
/// empty, and no route, root field, catalog access, or background task exists —
/// a deployment without attachments is byte-for-byte unaffected.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<StorageBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<StorageSigning>,
    #[serde(default, skip_serializing_if = "StorageGc::is_default")]
    pub gc: StorageGc,
    /// What one session may ask for. Without these a caller can mint upload
    /// rows as fast as it can send requests.
    #[serde(default, skip_serializing_if = "StorageLimits::is_default")]
    pub limits: StorageLimits,
    /// Which session variable identifies the uploader.
    #[serde(default, skip_serializing_if = "StorageIdentity::is_default")]
    pub identity: StorageIdentity,
    /// Browser origins allowed to upload and download directly.
    #[serde(default, skip_serializing_if = "StorageCors::is_empty")]
    pub cors: StorageCors,
}

impl StorageMetadata {
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    pub fn backend(&self, name: &str) -> Option<&StorageBackend> {
        self.backends.iter().find(|b| b.name() == name)
    }
}

/// One resolved attachment: the table-local declaration together with the
/// source and table it was declared on. This is what every consumer downstream
/// of metadata works with, so nobody re-walks the source tree.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedAttachment<'a> {
    pub source: &'a str,
    pub table: &'a QualifiedTable,
    pub attachment: &'a Attachment,
}

impl ResolvedAttachment<'_> {
    /// `<schema>.<table>.<column>` — the stable identity recorded on every
    /// upload row and used as the GraphQL enum value.
    pub fn key(&self) -> String {
        format!("{}.{}", self.table, self.attachment.column)
    }
}

impl Metadata {
    /// Every file column declared anywhere in the metadata, in source and then
    /// table declaration order.
    pub fn attachments(&self) -> impl Iterator<Item = ResolvedAttachment<'_>> {
        self.sources.iter().flat_map(|source| {
            source.tables.iter().flat_map(move |table| {
                table
                    .attachments
                    .iter()
                    .map(move |attachment| ResolvedAttachment {
                        source: &source.name,
                        table: &table.table,
                        attachment,
                    })
            })
        })
    }

    /// Look one up by its `<schema>.<table>.<column>` key.
    pub fn attachment(&self, key: &str) -> Option<ResolvedAttachment<'_>> {
        self.attachments().find(|a| a.key() == key)
    }
}

/// One deployment-configured place to put bytes. `kind` selects the compiled
/// implementation; metadata can never supply code or a request shape.
/// Where bytes go. The enum has one variant today, and stays an enum because
/// the tag is what a deployment writes: adding a second store must not change
/// the shape of the first.
///
/// There is deliberately no local-disk store. Serving bytes from the engine's
/// own origin put caller-supplied content next to the GraphQL API, made the
/// engine a file server on the request path, and needed a second signing
/// scheme, a second download route, and a traversal guard — all to reimplement
/// what an object store already does.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageBackend {
    S3(S3Backend),
}

impl StorageBackend {
    pub fn name(&self) -> &str {
        match self {
            StorageBackend::S3(b) => &b.name,
        }
    }
}

/// An S3-compatible bucket. `endpoint` and `path_style` cover MinIO and the
/// other compatible hosts; credentials are [`SecretRef`] only, never literals.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct S3Backend {
    pub name: String,
    pub bucket: String,
    pub region: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub path_style: bool,
    pub access_key_id: SecretRef,
    pub secret_access_key: SecretRef,
    /// Where a **public** attachment's stable URL is rooted: a CDN
    /// distribution, or the bucket's own public origin. Required before an
    /// attachment on this backend may be declared public — the engine will not
    /// guess that a bucket is world-readable, and a guessed origin would
    /// publish links that 403.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
}

/// URL lifetimes, and the secret behind the one URL the engine serves itself.
///
/// Upload and download URLs are presigned by the object store, but the call a
/// client makes to report an upload finished is answered by the engine, and
/// that capability carries no other proof.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSigning {
    pub secret: SecretRef,
    #[serde(default = "default_upload_ttl_seconds")]
    pub upload_ttl_seconds: u32,
    #[serde(default = "default_download_ttl_seconds")]
    pub download_ttl_seconds: u32,
}

fn default_upload_ttl_seconds() -> u32 {
    900
}

fn default_download_ttl_seconds() -> u32 {
    300
}

/// One column of this table holds a file reference. Many files per entity is an
/// ordinary child table carrying such a column.
///
/// There is deliberately no role list here. Who may upload into the column is
/// exactly who may write it — the table's own `insert_permissions` and
/// `update_permissions`, resolved through inherited roles like every other
/// write — and who may read a download URL is who its `select_permissions`
/// already let read the column. A second role list beside those would be a
/// second authorization model to keep in sync.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub column: String,
    pub backend: String,
    pub max_bytes: u64,
    /// Exact media types, no wildcards. Empty accepts any type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_types: Vec<String>,
    /// The stored bytes are world-readable.
    ///
    /// A public attachment is served from a stable, immutable, unsigned URL, so
    /// a CDN and a browser can cache it forever and a subscription never sees
    /// it change. That is a real grant, never inferred: anyone holding the URL
    /// reads the file regardless of the row's select permission, which still
    /// governs who can *discover* the URL.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub public: bool,
}

impl Attachment {
    pub fn allows_media_type(&self, media_type: &str) -> bool {
        self.media_types.is_empty() || self.media_types.iter().any(|m| m == media_type)
    }
}

/// Collector windows. Every window is a whole number of days and defaults to
/// one, which is the interval the feature was asked for.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageGc {
    #[serde(default = "default_gc_days")]
    pub every_days: u32,
    /// How long an upload nobody claimed is kept past its expiry.
    #[serde(default = "default_gc_days")]
    pub pending_ttl_days: u32,
    /// How long a claimed object no row references any more is kept.
    #[serde(default = "default_gc_days")]
    pub orphan_grace_days: u32,
}

fn default_gc_days() -> u32 {
    1
}

/// What one session may ask of the upload surface.
///
/// The engine does no network-level rate limiting anywhere — that belongs to
/// the deployment's reverse proxy. These are the two limits a proxy cannot
/// express, because both are counted against rows the engine owns.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageLimits {
    /// Unclaimed uploads one session may hold at a time.
    #[serde(default = "default_pending_uploads")]
    pub pending_uploads_per_session: u32,
    /// Upload URLs one session may mint per minute.
    #[serde(default = "default_uploads_per_minute")]
    pub uploads_per_minute_per_session: u32,
}

fn default_pending_uploads() -> u32 {
    20
}

fn default_uploads_per_minute() -> u32 {
    60
}

impl Default for StorageLimits {
    fn default() -> Self {
        Self {
            pending_uploads_per_session: default_pending_uploads(),
            uploads_per_minute_per_session: default_uploads_per_minute(),
        }
    }
}

impl StorageLimits {
    fn is_default(&self) -> bool {
        self.pending_uploads_per_session == default_pending_uploads()
            && self.uploads_per_minute_per_session == default_uploads_per_minute()
    }
}

/// Which session variable identifies the uploader.
///
/// An upload is bound to the session that asked for it, and this says what
/// "the session" means. A deployment that identifies tenants by something other
/// than a user id must say so, or the binding silently weakens to the role.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageIdentity {
    #[serde(default = "default_identity_variable")]
    pub session_variable: String,
}

fn default_identity_variable() -> String {
    "x-donat-user-id".to_string()
}

impl Default for StorageIdentity {
    fn default() -> Self {
        Self {
            session_variable: default_identity_variable(),
        }
    }
}

impl StorageIdentity {
    fn is_default(&self) -> bool {
        self.session_variable == default_identity_variable()
    }
}

/// Browser origins allowed to reach the file routes directly.
///
/// A browser uploading to a signed URL does so cross-origin, and no other
/// engine surface needs this, so it is declared here rather than globally.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCors {
    /// Exact origins, or a single `"*"`. Empty mounts no CORS at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_origins: Vec<String>,
    #[serde(default = "default_cors_max_age")]
    pub max_age_seconds: u32,
}

fn default_cors_max_age() -> u32 {
    600
}

impl StorageCors {
    pub fn is_empty(&self) -> bool {
        self.allow_origins.is_empty()
    }

    pub fn allows(&self, origin: &str) -> bool {
        self.allow_origins.iter().any(|o| o == "*" || o == origin)
    }
}

impl Default for StorageGc {
    fn default() -> Self {
        Self {
            every_days: default_gc_days(),
            pending_ttl_days: default_gc_days(),
            orphan_grace_days: default_gc_days(),
        }
    }
}

impl StorageGc {
    fn is_default(&self) -> bool {
        self.every_days == default_gc_days()
            && self.pending_ttl_days == default_gc_days()
            && self.orphan_grace_days == default_gc_days()
    }
}

#[cfg(test)]
mod endpoint_authentication_tests {
    use super::*;

    /// The shape a user writes for a signature-authenticated callback, in the
    /// spelling the spec documents. If this stops deserializing, the spec is
    /// wrong or the format broke — either way it is not a detail.
    #[test]
    fn signature_scheme_round_trips_from_yaml() {
        let yaml = r#"
name: stripe_events
url: hooks/stripe
methods: [POST]
definition:
  query:
    collection_name: hooks
    query_name: RecordStripeInvoiceEvent
authenticate:
  signature:
    header: Stripe-Signature
    algorithm: hmac_sha256
    encoding: hex
    signed_payload: "{timestamp}.{body}"
    timestamp:
      signature_header_field:
        field: t
    tolerance_seconds: 300
    secret:
      value_from_env: STRIPE_WEBHOOK_SECRET
  run_as: billing
  max_body_bytes: 65536
  accept:
    - json_pointer: /type
      equals: invoice.paid
"#;
        let endpoint: RestEndpoint = serde_yaml::from_str(yaml).expect("endpoint parses");
        let auth = endpoint.authenticate.expect("authenticate present");
        assert_eq!(auth.run_as, "billing");
        assert_eq!(auth.max_body_bytes, 65_536);
        assert_eq!(auth.accept.len(), 1);
        assert_eq!(auth.accept[0].json_pointer, "/type");
        match auth.credential {
            EndpointCredential::Signature(scheme) => {
                assert_eq!(scheme.header, "Stripe-Signature");
                assert_eq!(scheme.algorithm, SignatureAlgorithm::HmacSha256);
                assert_eq!(scheme.encoding, SignatureEncoding::Hex);
                assert_eq!(scheme.signed_payload, "{timestamp}.{body}");
                assert_eq!(scheme.tolerance_seconds, Some(300));
                assert_eq!(scheme.secret.value_from_env, "STRIPE_WEBHOOK_SECRET");
            }
            other => panic!("expected a signature scheme, got {other:?}"),
        }
    }

    /// The defaults are the safe reading: a body bound even when nobody said
    /// so, hex encoding, and the whole body signed.
    #[test]
    fn signature_defaults_are_the_conservative_ones() {
        let yaml = r#"
signature:
  header: X-Hub-Signature-256
  algorithm: hmac_sha256
  secret:
    value_from_env: GITHUB_WEBHOOK_SECRET
run_as: integrations
"#;
        let auth: EndpointAuthentication = serde_yaml::from_str(yaml).expect("parses");
        assert_eq!(auth.max_body_bytes, 65_536);
        assert!(auth.accept.is_empty(), "no accept list means accept all");
        match auth.credential {
            EndpointCredential::Signature(s) => {
                assert_eq!(s.encoding, SignatureEncoding::Hex);
                assert_eq!(s.signed_payload, "{body}");
                assert!(s.timestamp.is_none());
                assert!(
                    s.tolerance_seconds.is_none(),
                    "absent tolerance stays absent rather than inventing a window"
                );
            }
            other => panic!("expected a signature scheme, got {other:?}"),
        }
    }

    #[test]
    fn shared_secret_is_the_other_credential() {
        let yaml = r#"
shared_secret:
  header: X-Api-Key
  secret:
    value_from_env: PARTNER_KEY
run_as: partner
"#;
        let auth: EndpointAuthentication = serde_yaml::from_str(yaml).expect("parses");
        match auth.credential {
            EndpointCredential::SharedSecret(s) => {
                assert_eq!(s.header, "X-Api-Key");
                assert_eq!(s.secret.value_from_env, "PARTNER_KEY");
            }
            other => panic!("expected a shared secret, got {other:?}"),
        }
    }

    /// An endpoint with no `authenticate` block is the existing behaviour, and
    /// every deployment has these. Silence here means the role still comes
    /// from headers.
    #[test]
    fn absent_authentication_is_the_existing_endpoint() {
        let yaml = r#"
name: list_products
url: products
methods: [GET]
definition:
  query:
    collection_name: petshop
    query_name: Products
"#;
        let endpoint: RestEndpoint = serde_yaml::from_str(yaml).expect("parses");
        assert!(endpoint.authenticate.is_none());
    }

    /// A misspelled key is a deploy failure rather than a silently ignored
    /// one — the difference between an endpoint that is unauthenticated and an
    /// endpoint that only looks authenticated.
    #[test]
    fn unknown_keys_are_refused() {
        let yaml = r#"
signature:
  header: X-Sig
  algorithm: hmac_sha256
  secret:
    value_from_env: K
  toleranceSeconds: 300
run_as: billing
"#;
        let parsed: Result<EndpointAuthentication, _> = serde_yaml::from_str(yaml);
        assert!(parsed.is_err(), "a camelCase near-miss must not be ignored");
    }

    #[test]
    fn a_credential_is_required() {
        let yaml = "run_as: billing\n";
        let parsed: Result<EndpointAuthentication, _> = serde_yaml::from_str(yaml);
        assert!(
            parsed.is_err(),
            "run_as with no credential would authenticate nobody as a role"
        );
    }
}
