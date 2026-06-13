//! Typed model of the Hasura v2 metadata format (metadata directory version 3).
//!
//! Field names and shapes follow the v2 spec so that exported Hasura metadata
//! (and the fixtures from `server/tests-py`) deserialize without translation.
//! Open-ended expressions (boolean filters, column presets) are kept as
//! `serde_json::Value` for now; they get a typed AST when the sqlgen
//! milestone needs to compile them.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

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
/// "none"), as Hasura's exported action metadata sometimes does.
fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
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
    pub fields: Vec<CustomTypeField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectType {
    pub name: String,
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceConfiguration {
    pub connection_info: ConnectionInfo,
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
}

impl QualifiedTable {
    pub fn schema(&self) -> &str {
        match self {
            QualifiedTable::Name(_) => "public",
            QualifiedTable::Qualified { schema, .. } => schema,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            QualifiedTable::Name(name) => name,
            QualifiedTable::Qualified { name, .. } => name,
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
    pub column_config: BTreeMap<String, serde_json::Value>,
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
}

/// A field joined to a remote schema: per-row arguments from columns.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteRelationship {
    pub name: String,
    #[serde(default)]
    pub hasura_fields: Vec<String>,
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

/// Boolean expression over rows (`{ author_id: { _eq: X-Hasura-User-Id } }`).
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Columns {
    Star,
    List(Vec<String>),
}

impl Default for Columns {
    fn default() -> Self {
        Columns::Star
    }
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
