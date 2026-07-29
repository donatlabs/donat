//! Typed model of the Donat v2 metadata format (metadata directory version 3).
//!
//! Field names and shapes follow the v2 spec so that exported Donat metadata
//! (and the fixtures from `server/tests-py`) deserialize without translation.
//! Open-ended expressions (boolean filters, column presets) are kept as
//! `serde_json::Value` for now; they get a typed AST when the sqlgen
//! milestone needs to compile them.

use std::collections::BTreeMap;
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
}

/// One named deployment instance of a connector module compiled into the
/// serving binary. The metadata can select only an instance and a module name;
/// it cannot provide code, a package, or another runtime implementation.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<ConnectorStaticHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_statuses: Vec<u16>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response: BTreeMap<String, ConnectorResponseBinding>,
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

    pub fn get(&self, name: &str) -> Option<&CommandValue> {
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
    pub value: CommandValue,
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

/// A typed command argument. The canonical metadata form is an ordered list;
/// the accepted mapping shorthand normalizes to this list during loading.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[serde(untagged)]
pub enum CommandStepOperation {
    SelectOne { select_one: SelectOneCommandStep },
    Insert { insert: InsertCommandStep },
    InsertMany { insert_many: InsertManyCommandStep },
    Update { update: UpdateCommandStep },
    Delete { delete: DeleteCommandStep },
    Assert { assert: AssertCommandStep },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SelectOneCommandStep {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default = "default_true")]
    pub require_found: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InsertCommandStep {
    pub table: QualifiedTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InsertManyCommandStep {
    pub table: QualifiedTable,
    pub for_each: CommandValue,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub object: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returning: Vec<String>,
    #[serde(default)]
    pub allow_empty: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
pub struct AssertCommandStep {
    pub rule: String,
    #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A closed, SQL-free reference used by command steps, results, guards, and
/// process effects. No variant can carry a SQL fragment or identifier template.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
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
    },
    Literal {
        literal: serde_json::Value,
    },
    Rule {
        rule: String,
        #[serde(rename = "with", default, skip_serializing_if = "BTreeMap::is_empty")]
        bindings: BTreeMap<String, CommandValue>,
    },
    SessionVariable {
        session_variable: String,
    },
}

/// Optional replay protection for a command. Its scope is deliberately typed
/// so a later validator can reject non-deterministic declarations.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandIdempotency {
    pub key: CommandIdempotencyKey,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<CommandIdempotencyScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
}

/// An idempotency key is intentionally separate from an ordinary command
/// value: command objects use `{ arg: ... }`, while the canonical
/// idempotency surface uses `{ argument: ... }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandIdempotencyKey {
    Argument { argument: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandIdempotencyScope {
    Argument { argument: String },
    SessionVariable { session_variable: String },
}

/// A durable hand-off requested by a command. It is only metadata in this
/// slice; no process rows, runtime calls, or mutation behavior are created.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandEffect {
    StartProcess { start_process: StartProcessEffect },
    SignalProcess { signal_process: SignalProcessEffect },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StartProcessEffect {
    pub process: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input: BTreeMap<String, CommandValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<CommandIdempotencyKey>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

/// One finite named object or enum declaration from `rules.yaml`. Validation
/// that exactly one body is present and that references form an acyclic graph
/// belongs to deploy-time catalog compilation, where metadata paths are known.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleTypeDeclaration {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<BTreeMap<String, String>>,
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
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
    pub handler: String,
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

/// `table: foo` or `table: { schema: public, name: foo }`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(untagged)]
pub enum QualifiedTable {
    Name(String),
    Qualified { schema: String, name: String },
    Parts(Vec<String>),
}

impl QualifiedTable {
    pub fn schema(&self) -> &str {
        match self {
            QualifiedTable::Name(_) => "public",
            QualifiedTable::Qualified { schema, .. } => schema,
            QualifiedTable::Parts(parts) => parts.first().map(String::as_str).unwrap_or("public"),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            QualifiedTable::Name(name) => name,
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
    /// Webhooks fired on row insert/update/delete (Donat event triggers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_triggers: Vec<EventTrigger>,
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
