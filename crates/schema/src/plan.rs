//! Query planning: one GraphQL operation -> Vec<RootField> (IR).

use std::collections::HashMap;

use donat_catalog::{Catalog, TableInfo};
use donat_ir::*;
use donat_metadata::{Columns, Metadata, QualifiedTable, SelectPermission, TableEntry};
use graphql_parser::query::{
    Definition, Document, Field as GqlField, OperationDefinition, Selection, SelectionSet,
    Value as GqlValue,
};
use serde_json::{Map as JsonMap, Value as Json};

use crate::naming::{column_db_name, column_graphql_name, root_names, table_base_name};

/// Per-request session: an explicit role + X-Donat-*/X-Hasura-* variables
/// (keys lower-cased). There is no admin role — every access goes through an
/// explicit per-role permission.
#[derive(Debug, Clone)]
pub struct Session {
    pub role: String,
    pub vars: HashMap<String, String>,
    /// True when x-donat-use-backend-only-permissions enables
    /// backend_only mutation permissions for this request.
    pub backend_request: bool,
}

impl Session {
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }
}

pub(crate) fn is_session_var_name(name: &str) -> bool {
    name.get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-donat-"))
        || name
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-hasura-"))
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PlanError {
    pub message: String,
    pub code: &'static str,
    pub path: String,
}

impl PlanError {
    pub fn new(path: &str, code: &'static str, message: impl Into<String>) -> Self {
        PlanError {
            message: message.into(),
            code,
            path: path.to_string(),
        }
    }

    pub fn validation(path: &str, message: impl Into<String>) -> Self {
        Self::new(path, "validation-failed", message)
    }

    /// Donat v2 GraphQL error body.
    pub fn to_graphql(&self) -> Json {
        serde_json::json!({
            "errors": [{
                "extensions": { "path": self.path, "code": self.code },
                "message": self.message,
            }]
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootKind {
    Select,
    ByPk,
    Aggregate,
}

/// What a root field reads from: a tracked table or a tracked function
/// returning rows of a tracked table.
#[derive(Debug, Clone, Copy)]
enum RootSource {
    Table(usize),
    Function(usize),
}

/// A planned operation, ready for sqlgen.
#[derive(Debug, Clone)]
pub enum Plan {
    Query(Vec<RootField>),
    /// Mutation root fields, executed sequentially in one transaction.
    Mutation(Vec<MutationRoot>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationKind {
    Insert,
    InsertOne,
    Update,
    UpdateByPk,
    Delete,
    DeleteByPk,
}

pub(crate) type Fragments<'a> =
    HashMap<String, &'a graphql_parser::query::FragmentDefinition<'static, String>>;

/// Planning context for a tracked table resolved against the catalog.
/// `perm: None` marks a permission-filter context: the bool_exp being
/// parsed was authored in metadata, so it may reference any column or
/// relationship regardless of the role's select permission.
pub(crate) struct TableCtx<'a> {
    pub(crate) entry: &'a TableEntry,
    pub(crate) info: &'a TableInfo,
    /// The select permissions in effect: empty = permission-filter context
    /// (anything goes); one = a plain role; several = an inherited role
    /// combining its parents.
    pub(crate) perms: Vec<&'a SelectPermission>,
    pub(crate) type_name: String,
}

fn perm_grants_column(p: &SelectPermission, name: &str) -> bool {
    match &p.columns {
        Columns::Star => true,
        Columns::List(cols) => cols.iter().any(|c| c == name),
    }
}

impl<'a> TableCtx<'a> {
    /// The effective select-permission limit: the max across parents
    /// (None as soon as any parent is unlimited).
    pub(crate) fn select_perm_limit(&self) -> Option<u64> {
        if self.perms.is_empty() {
            return None;
        }
        let mut max = 0u64;
        for p in &self.perms {
            match p.limit {
                None => return None,
                Some(l) => max = max.max(l),
            }
        }
        Some(max)
    }

    pub(crate) fn allow_aggregations(&self) -> bool {
        self.perms.iter().any(|p| p.allow_aggregations)
    }

    pub(crate) fn computed_field_allowed(&self, name: &str) -> bool {
        self.perms
            .iter()
            .any(|p| p.computed_fields.iter().any(|c| c == name))
    }

    /// Parents granting this column (for cell-level guards).
    pub(crate) fn granting_perms(&self, name: &str) -> Vec<&'a SelectPermission> {
        self.perms
            .iter()
            .copied()
            .filter(|p| perm_grants_column(p, name))
            .collect()
    }

    pub(crate) fn column_allowed(&self, name: &str) -> bool {
        if self.info.column(name).is_none() {
            return false;
        }
        if self.perms.is_empty() {
            return true;
        }
        self.perms.iter().any(|p| perm_grants_column(p, name))
    }

    /// Whether the role may select at least one column. Roles with
    /// `columns: []` still aggregate via count, but Donat omits
    /// column-typed arguments (e.g. `count(columns:)`) from their schema.
    pub(crate) fn any_column_allowed(&self) -> bool {
        self.info
            .columns
            .iter()
            .any(|c| self.column_allowed(&c.name))
    }

    pub(crate) fn column_allowed_for_filter(&self, name: &str, is_permission: bool) -> bool {
        self.column_db_name_for_filter(name, is_permission)
            .is_some_and(|db_name| {
                if is_permission {
                    self.info.column(&db_name).is_some()
                } else {
                    self.column_allowed(&db_name)
                }
            })
    }

    pub(crate) fn column_info(&self, name: &str) -> Option<&'a donat_catalog::ColumnInfo> {
        self.info.column(name)
    }

    pub(crate) fn column_graphql_name(&self, db_name: &str) -> String {
        column_graphql_name(self.entry, db_name)
    }

    pub(crate) fn column_db_name(&self, graphql_name: &str) -> Option<String> {
        let db_name = column_db_name(self.entry, graphql_name);
        self.info.column(&db_name).map(|_| db_name)
    }

    pub(crate) fn column_db_name_for_filter(
        &self,
        name: &str,
        is_permission: bool,
    ) -> Option<String> {
        if is_permission && self.info.column(name).is_some() {
            Some(name.to_string())
        } else {
            self.column_db_name(name)
        }
    }
}

pub struct Planner<'a> {
    /// When false (DONAT_GRAPHQL_INFER_FUNCTION_PERMISSIONS=false),
    /// tracked functions need an explicit per-role permission entry.
    pub infer_function_permissions: bool,
    /// Relay mode (/v1beta1/relay): `<t>_connection` roots, global ids.
    pub relay: bool,
    inherited_roles: &'a [donat_metadata::InheritedRole],
    remote_schemas: &'a [donat_metadata::RemoteSchema],
    catalog: &'a Catalog,
    tables: &'a [TableEntry],
    functions: &'a [donat_metadata::FunctionEntry],
    /// "schema.name" -> index into `tables`.
    by_table: HashMap<String, usize>,
    /// root field name -> (kind, source).
    roots: HashMap<String, (RootKind, RootSource)>,
    /// mutation root field name -> (kind, table index).
    pub(crate) mutation_roots: HashMap<String, (MutationKind, usize)>,
    /// mutation root field name -> function index (exposed_as: mutation).
    mutation_function_roots: HashMap<String, usize>,
}

impl<'a> Planner<'a> {
    pub fn new(metadata: &'a Metadata, catalog: &'a Catalog) -> Self {
        let (tables, functions): (&[TableEntry], &[donat_metadata::FunctionEntry]) = metadata
            .sources
            .first()
            .map(|s| (s.tables.as_slice(), s.functions.as_slice()))
            .unwrap_or((&[], &[]));

        let mut by_table = HashMap::new();
        let mut roots = HashMap::new();
        for (idx, entry) in tables.iter().enumerate() {
            by_table.insert(
                format!("{}.{}", entry.table.schema(), entry.table.name()),
                idx,
            );
            let names = root_names(entry);
            roots.insert(names.select, (RootKind::Select, RootSource::Table(idx)));
            roots.insert(names.select_by_pk, (RootKind::ByPk, RootSource::Table(idx)));
            roots.insert(
                names.select_aggregate,
                (RootKind::Aggregate, RootSource::Table(idx)),
            );
        }
        let mut mutation_function_roots = HashMap::new();
        for (idx, entry) in functions.iter().enumerate() {
            let base = entry
                .configuration
                .as_ref()
                .and_then(|c| c.custom_name.clone())
                .unwrap_or_else(|| crate::naming::default_base_name(&entry.function));
            let as_mutation = entry
                .configuration
                .as_ref()
                .and_then(|c| c.exposed_as.as_deref())
                == Some("mutation");
            if as_mutation {
                mutation_function_roots.insert(base, idx);
            } else {
                roots.insert(base.clone(), (RootKind::Select, RootSource::Function(idx)));
                roots.insert(
                    format!("{base}_aggregate"),
                    (RootKind::Aggregate, RootSource::Function(idx)),
                );
            }
        }

        let mut mutation_roots = HashMap::new();
        for (idx, entry) in tables.iter().enumerate() {
            let base = table_base_name(entry);
            let custom = entry.configuration.as_ref().map(|c| &c.custom_root_fields);
            let get = |key: &str, default: String| -> String {
                custom.and_then(|m| m.get(key).cloned()).unwrap_or(default)
            };
            mutation_roots.insert(
                get("insert", format!("insert_{base}")),
                (MutationKind::Insert, idx),
            );
            mutation_roots.insert(
                get("insert_one", format!("insert_{base}_one")),
                (MutationKind::InsertOne, idx),
            );
            mutation_roots.insert(
                get("update", format!("update_{base}")),
                (MutationKind::Update, idx),
            );
            mutation_roots.insert(
                get("update_by_pk", format!("update_{base}_by_pk")),
                (MutationKind::UpdateByPk, idx),
            );
            mutation_roots.insert(
                get("delete", format!("delete_{base}")),
                (MutationKind::Delete, idx),
            );
            mutation_roots.insert(
                get("delete_by_pk", format!("delete_{base}_by_pk")),
                (MutationKind::DeleteByPk, idx),
            );
        }

        Planner {
            infer_function_permissions: true,
            relay: false,
            inherited_roles: &metadata.inherited_roles,
            remote_schemas: &metadata.remote_schemas,
            catalog,
            tables,
            functions,
            by_table,
            roots,
            mutation_roots,
            mutation_function_roots,
        }
    }

    /// Resolve a tracked table for a role: entry + catalog info + select
    /// permission. `None` means "does not exist for this role".
    pub(crate) fn table_ctx(&self, idx: usize, role: &str) -> Option<TableCtx<'a>> {
        let entry = &self.tables[idx];
        let info = self
            .catalog
            .table(entry.table.schema(), entry.table.name())?;
        let perms = self.role_select_perms(entry, role);
        if perms.is_empty() {
            return None;
        }
        Some(TableCtx {
            entry,
            info,
            perms,
            type_name: table_base_name(entry),
        })
    }

    /// The select permissions a (possibly inherited) role has on a table:
    /// a direct permission overrides the inherited combination.
    fn role_select_perms(&self, entry: &'a TableEntry, role: &str) -> Vec<&'a SelectPermission> {
        if let Some(p) = entry.select_permissions.iter().find(|p| p.role == role) {
            return vec![&p.permission];
        }
        let mut out = vec![];
        for parent in self.expand_role(role) {
            if let Some(p) = entry.select_permissions.iter().find(|p| p.role == parent) {
                out.push(&p.permission);
            }
        }
        out
    }

    /// Resolve a non-select (mutation/function) permission for a role:
    /// a direct permission wins; otherwise the *immediate* parents'
    /// resolved permissions are inherited only when they don't conflict
    /// (i.e. all identical). Resolution is per level: a parent whose own
    /// parents conflict simply contributes nothing.
    pub(crate) fn resolve_role_perm<'x, T: serde::Serialize>(
        &self,
        list: &'x [donat_metadata::PermissionEntry<T>],
        role: &str,
        applies: impl Fn(&T) -> bool,
    ) -> Option<&'x T> {
        let mut visiting = std::collections::HashSet::new();
        self.resolve_role_perm_rec(list, role, &applies, &mut visiting)
    }

    fn resolve_role_perm_rec<'x, T: serde::Serialize>(
        &self,
        list: &'x [donat_metadata::PermissionEntry<T>],
        role: &str,
        applies: &impl Fn(&T) -> bool,
        visiting: &mut std::collections::HashSet<String>,
    ) -> Option<&'x T> {
        if let Some(p) = list
            .iter()
            .find(|p| p.role == role && applies(&p.permission))
        {
            return Some(&p.permission);
        }
        if !visiting.insert(role.to_string()) {
            return None;
        }
        let inherited = self.inherited_roles.iter().find(|r| r.role_name == role)?;
        let mut found: Vec<&T> = vec![];
        for parent in &inherited.role_set {
            if let Some(p) = self.resolve_role_perm_rec(list, parent, applies, visiting) {
                found.push(p);
            }
        }
        visiting.remove(role);
        match found.len() {
            0 => None,
            1 => Some(found[0]),
            _ => {
                let first = serde_json::to_value(found[0]).ok();
                if found.iter().all(|p| serde_json::to_value(p).ok() == first) {
                    Some(found[0])
                } else {
                    None
                }
            }
        }
    }

    /// Is the function permitted for the role (directly or via any
    /// inherited ancestor)? Always true when permissions are inferred.
    fn function_allowed(&self, fentry: &donat_metadata::FunctionEntry, role: &str) -> bool {
        if self.infer_function_permissions {
            return true;
        }
        if fentry.permissions.iter().any(|p| p.role == role) {
            return true;
        }
        self.expand_role(role)
            .iter()
            .any(|parent| fentry.permissions.iter().any(|p| &p.role == parent))
    }

    /// Plan a tracked-function mutation root, if `name` is one.
    pub(crate) fn try_plan_function_mutation(
        &self,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Option<Result<SelectQuery, PlanError>> {
        let &fidx = self.mutation_function_roots.get(&field.name)?;
        let fentry = &self.functions[fidx];
        if !self.function_allowed(fentry, &session.role) {
            return None;
        }
        let Some(finfo) = self
            .catalog
            .function(fentry.function.schema(), fentry.function.name())
        else {
            return None;
        };
        let Some((rschema, rname)) = &finfo.returns_table else {
            return None;
        };
        let remote = QualifiedTable::Qualified {
            schema: rschema.clone(),
            name: rname.clone(),
        };
        let ctx = self.table_ctx_by_name(&remote, &session.role)?;
        Some((|| {
            let from = self.function_from(fentry, finfo, field, vars, session, path)?;
            self.build_select(
                &ctx,
                RootKind::Select,
                from,
                field,
                fragments,
                vars,
                session,
                path,
            )
        })())
    }

    /// Any function mutation available to the role?
    pub(crate) fn role_has_function_mutation(&self, role: &str) -> bool {
        self.mutation_function_roots
            .values()
            .any(|&idx| self.function_allowed(&self.functions[idx], role))
    }

    /// Conflicting (non-inheritable) mutation permissions of inherited
    /// roles: (role, table name, permission kind).
    pub fn mutation_permission_conflicts(&self) -> Vec<(String, String, &'static str)> {
        let mut out = vec![];
        for role in self.inherited_roles {
            for entry in self.tables {
                if self.conflicts_in(&entry.insert_permissions, &role.role_name) {
                    out.push((
                        role.role_name.clone(),
                        entry.table.name().to_string(),
                        "insert",
                    ));
                }
                if self.conflicts_in(&entry.update_permissions, &role.role_name) {
                    out.push((
                        role.role_name.clone(),
                        entry.table.name().to_string(),
                        "update",
                    ));
                }
                if self.conflicts_in(&entry.delete_permissions, &role.role_name) {
                    out.push((
                        role.role_name.clone(),
                        entry.table.name().to_string(),
                        "delete",
                    ));
                }
            }
        }
        out
    }

    fn conflicts_in<T: serde::Serialize>(
        &self,
        list: &[donat_metadata::PermissionEntry<T>],
        role: &str,
    ) -> bool {
        if list.iter().any(|p| p.role == role) {
            return false; // a direct permission overrides the parents
        }
        let Some(inherited) = self.inherited_roles.iter().find(|r| r.role_name == role) else {
            return false;
        };
        let mut found = vec![];
        for parent in &inherited.role_set {
            let mut visiting = std::collections::HashSet::new();
            if let Some(p) = self.resolve_role_perm_rec(list, parent, &|_| true, &mut visiting) {
                found.push(p);
            }
        }
        found.len() > 1 && {
            let first = serde_json::to_value(found[0]).ok();
            !found.iter().all(|p| serde_json::to_value(p).ok() == first)
        }
    }

    /// Does the role have any direct or parent permission in the list,
    /// even a conflicting one? (Conflicts hide the field but keep the
    /// mutation root non-empty.)
    pub(crate) fn any_role_perm<T>(
        &self,
        list: &[donat_metadata::PermissionEntry<T>],
        role: &str,
    ) -> bool {
        if list.iter().any(|p| p.role == role) {
            return true;
        }
        self.expand_role(role)
            .iter()
            .any(|parent| list.iter().any(|p| &p.role == parent))
    }

    /// Expand an inherited role into concrete parent roles (recursively,
    /// cycle-safe). A non-inherited role expands to nothing.
    pub(crate) fn expand_role(&self, role: &str) -> Vec<String> {
        let mut out = vec![];
        let mut stack = vec![role.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            match self.inherited_roles.iter().find(|r| r.role_name == current) {
                Some(inherited) => {
                    for parent in &inherited.role_set {
                        stack.push(parent.clone());
                    }
                }
                None => {
                    if current != role {
                        out.push(current);
                    }
                }
            }
        }
        out
    }

    /// A permission-filter context: no select permission required.
    fn filter_ctx(&self, idx: usize) -> Option<TableCtx<'a>> {
        let entry = &self.tables[idx];
        let info = self
            .catalog
            .table(entry.table.schema(), entry.table.name())?;
        Some(TableCtx {
            entry,
            info,
            perms: vec![],
            type_name: table_base_name(entry),
        })
    }

    pub(crate) fn table_ctx_by_name(
        &self,
        table: &QualifiedTable,
        role: &str,
    ) -> Option<TableCtx<'a>> {
        let idx = *self
            .by_table
            .get(&format!("{}.{}", table.schema(), table.name()))?;
        self.table_ctx(idx, role)
    }

    /// Context for traversing a relationship inside a bool_exp.
    pub(crate) fn relationship_ctx(
        &self,
        table: &QualifiedTable,
        session: &Session,
        is_permission: bool,
    ) -> Option<TableCtx<'a>> {
        let idx = *self
            .by_table
            .get(&format!("{}.{}", table.schema(), table.name()))?;
        if is_permission {
            self.filter_ctx(idx)
        } else {
            self.table_ctx(idx, &session.role)
        }
    }

    pub(crate) fn tables(&self) -> &'a [TableEntry] {
        self.tables
    }

    pub(crate) fn entry_for(&self, table: &QualifiedTable) -> Option<&'a TableEntry> {
        let idx = *self
            .by_table
            .get(&format!("{}.{}", table.schema(), table.name()))?;
        Some(&self.tables[idx])
    }

    /// Argument list for a computed-field function call: the enclosing
    /// row, the session json, and any extra user-provided `args`.
    pub(crate) fn computed_field_args(
        &self,
        finfo: &donat_catalog::FunctionInfo,
        def: &donat_metadata::ComputedFieldDefinition,
        session: &Session,
        user_args: Option<&JsonMap<String, Json>>,
        path: &str,
    ) -> Result<Vec<RowFunctionArg>, PlanError> {
        finfo
            .args
            .iter()
            .map(|a| {
                let is_session = a
                    .name
                    .as_deref()
                    .zip(def.session_argument.as_deref())
                    .is_some_and(|(n, s)| n == s);
                if is_session {
                    return Ok(RowFunctionArg::SessionJson(session_json(session)));
                }
                if a.composite_of.is_some() {
                    return Ok(RowFunctionArg::Row);
                }
                let name = a.name.as_deref().unwrap_or_default();
                match user_args.and_then(|m| m.get(name)) {
                    Some(v) => Ok(RowFunctionArg::Value {
                        value: Scalar::Json(v.clone()),
                        pg_type: a.pg_type.clone(),
                    }),
                    None => Err(PlanError::validation(
                        path,
                        format!("missing function argument: \"{name}\""),
                    )),
                }
            })
            .collect()
    }

    /// Context for planning a mutation: no select permission required
    /// (the mutation-specific permission is checked by the caller).
    pub(crate) fn mutation_table_ctx(&self, idx: usize) -> Option<TableCtx<'a>> {
        self.filter_ctx(idx)
    }

    /// A permission-filter twin of an existing context.
    pub(crate) fn filter_ctx_of(&self, ctx: &TableCtx<'a>) -> TableCtx<'a> {
        TableCtx {
            entry: ctx.entry,
            info: ctx.info,
            perms: vec![],
            type_name: ctx.type_name.clone(),
        }
    }

    pub(crate) fn catalog_function(
        &self,
        schema: &str,
        name: &str,
    ) -> Option<&'a donat_catalog::FunctionInfo> {
        self.catalog.function(schema, name)
    }

    /// Resolve a relationship (object or array) by field name to
    /// (remote table, join pairs).
    pub(crate) fn relationship_target(
        &self,
        ctx: &TableCtx,
        name: &str,
        path: &str,
    ) -> Option<(QualifiedTable, Vec<(String, String)>)> {
        if let Some(rel) = ctx
            .entry
            .object_relationships
            .iter()
            .find(|r| r.name == name)
        {
            return self.object_rel_target(ctx, rel, path).ok();
        }
        if let Some(rel) = ctx
            .entry
            .array_relationships
            .iter()
            .find(|r| r.name == name)
        {
            return self.array_rel_target(ctx, rel, path).ok();
        }
        None
    }

    pub fn plan(
        &self,
        doc: &Document<'static, String>,
        operation_name: Option<&str>,
        variables: &JsonMap<String, Json>,
        session: &Session,
    ) -> Result<Plan, PlanError> {
        let mut fragments: Fragments = HashMap::new();
        let mut operations = vec![];
        for def in &doc.definitions {
            match def {
                Definition::Fragment(f) => {
                    fragments.insert(f.name.clone(), f);
                }
                Definition::Operation(op) => operations.push(op),
            }
        }

        let op = self.pick_operation(&operations, operation_name)?;

        let (selection_set, var_definitions, is_mutation) = match op {
            OperationDefinition::Query(q) => {
                (&q.selection_set, q.variable_definitions.as_slice(), false)
            }
            OperationDefinition::SelectionSet(s) => (s, [].as_slice(), false),
            OperationDefinition::Mutation(m) => {
                (&m.selection_set, m.variable_definitions.as_slice(), true)
            }
            // Subscriptions plan exactly like queries; the transport layer
            // decides delivery (currently: one snapshot per `start`).
            OperationDefinition::Subscription(s) => {
                (&s.selection_set, s.variable_definitions.as_slice(), false)
            }
        };

        // Effective variables: provided ones + defaults from the definition.
        let mut vars = variables.clone();
        for vd in var_definitions {
            if !vars.contains_key(&vd.name) {
                if let Some(default) = &vd.default_value {
                    vars.insert(vd.name.clone(), value_to_json(default, &vars, "$")?);
                }
            }
        }

        if is_mutation {
            return self
                .plan_mutation(selection_set, &fragments, &vars, session)
                .map(Plan::Mutation);
        }

        let mut out = vec![];
        for field in flatten(selection_set, &fragments, &vars, None)? {
            let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
            if field.name == "__typename" {
                out.push(RootField::Typename {
                    alias,
                    value: "query_root".to_string(),
                });
                continue;
            }
            let path = format!("$.selectionSet.{}", field.name);
            let not_found = || {
                PlanError::validation(
                    &path,
                    format!("field '{}' not found in type: 'query_root'", field.name),
                )
            };
            if self.relay {
                if let Some(root) =
                    self.plan_relay_root(field, &fragments, &vars, session, &path)?
                {
                    out.push(root);
                    continue;
                }
            }
            let Some(&(kind, source)) = self.roots.get(&field.name) else {
                return Err(not_found());
            };
            let (ctx, from) = match source {
                RootSource::Table(idx) => {
                    let Some(ctx) = self.table_ctx(idx, &session.role) else {
                        return Err(not_found());
                    };
                    // Donat omits <table>_by_pk from the role's schema unless
                    // the role may select every primary-key column.
                    if kind == RootKind::ByPk
                        && (ctx.info.primary_key.is_empty()
                            || !ctx.info.primary_key.iter().all(|c| ctx.column_allowed(c)))
                    {
                        return Err(not_found());
                    }
                    let from = FromSource::Table(Table {
                        schema: ctx.info.schema.clone(),
                        name: ctx.info.name.clone(),
                    });
                    (ctx, from)
                }
                RootSource::Function(fidx) => {
                    let fentry = &self.functions[fidx];
                    if !self.infer_function_permissions {
                        let direct = fentry.permissions.iter().any(|p| p.role == session.role);
                        let inherited = self
                            .expand_role(&session.role)
                            .iter()
                            .any(|parent| fentry.permissions.iter().any(|p| &p.role == parent));
                        if !direct && !inherited {
                            return Err(not_found());
                        }
                    }
                    let Some(finfo) = self
                        .catalog
                        .function(fentry.function.schema(), fentry.function.name())
                    else {
                        return Err(not_found());
                    };
                    let Some((rschema, rname)) = &finfo.returns_table else {
                        return Err(not_found());
                    };
                    let remote = QualifiedTable::Qualified {
                        schema: rschema.clone(),
                        name: rname.clone(),
                    };
                    let Some(ctx) = self.table_ctx_by_name(&remote, &session.role) else {
                        return Err(not_found());
                    };
                    let from = self.function_from(fentry, finfo, field, &vars, session, &path)?;
                    (ctx, from)
                }
            };
            if kind == RootKind::Aggregate && !ctx.allow_aggregations() {
                return Err(not_found());
            }
            let query =
                self.build_select(&ctx, kind, from, field, &fragments, &vars, session, &path)?;
            out.push(RootField::Select { alias, query });
        }
        if out.is_empty() {
            return Err(PlanError::validation("$", "selection set cannot be empty"));
        }
        Ok(Plan::Query(out))
    }

    fn pick_operation<'o>(
        &self,
        operations: &[&'o OperationDefinition<'static, String>],
        name: Option<&str>,
    ) -> Result<&'o OperationDefinition<'static, String>, PlanError> {
        match name {
            Some(wanted) => operations
                .iter()
                .find(|op| op_name(op) == Some(wanted))
                .copied()
                .ok_or_else(|| {
                    PlanError::validation(
                        "$",
                        format!("no such operation found in the document: \"{wanted}\""),
                    )
                }),
            None => {
                if operations.len() == 1 {
                    Ok(operations[0])
                } else {
                    Err(PlanError::validation(
                        "$",
                        "exactly one operation has to be present in the document when operationName is not specified",
                    ))
                }
            }
        }
    }

    /// Resolve a tracked function root: map GraphQL `args` onto the
    /// function's declared arguments (substituting the session argument).
    fn function_from(
        &self,
        fentry: &donat_metadata::FunctionEntry,
        finfo: &donat_catalog::FunctionInfo,
        field: &GqlField<'static, String>,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<FromSource, PlanError> {
        let session_argument = fentry
            .configuration
            .as_ref()
            .and_then(|c| c.session_argument.as_deref());

        let user_args = field
            .arguments
            .iter()
            .find(|(name, _)| name == "args")
            .map(|(_, v)| value_to_json(v, vars, path))
            .transpose()?
            .unwrap_or(Json::Object(JsonMap::new()));
        let user_args = user_args
            .as_object()
            .cloned()
            .ok_or_else(|| PlanError::validation(path, "expected an object for 'args'"))?;

        let mut args = vec![];
        for (i, arg) in finfo.args.iter().enumerate() {
            if let (Some(name), Some(sess_arg)) = (&arg.name, session_argument) {
                if name == sess_arg {
                    args.push(FunctionArgValue {
                        name: arg.name.clone(),
                        value: Scalar::Json(Json::String(session_json(session))),
                        pg_type: arg.pg_type.clone(),
                    });
                    continue;
                }
            }
            let value = arg.name.as_ref().and_then(|n| user_args.get(n)).cloned();
            match value {
                Some(value) => args.push(FunctionArgValue {
                    name: arg.name.clone(),
                    value: Scalar::Json(value),
                    pg_type: arg.pg_type.clone(),
                }),
                // Arguments with a DEFAULT may be omitted (named notation
                // keeps the remaining arguments unambiguous).
                None if arg.has_default => {}
                None => {
                    return Err(PlanError::validation(
                        path,
                        format!(
                            "missing function argument: \"{}\"",
                            arg.name.clone().unwrap_or_else(|| format!("${i}"))
                        ),
                    ));
                }
            }
        }

        Ok(FromSource::Function {
            schema: finfo.schema.clone(),
            name: finfo.name.clone(),
            args,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_select(
        &self,
        ctx: &TableCtx,
        kind: RootKind,
        from: FromSource,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<SelectQuery, PlanError> {
        let mut user_where = None;
        let mut order_by = vec![];
        let mut limit = None;
        let mut offset = None;
        let mut distinct_on: Vec<String> = vec![];
        let mut pk_predicate = vec![];

        for (arg_name, arg_value) in &field.arguments {
            let value = value_to_json(arg_value, vars, path)?;
            match (kind, arg_name.as_str()) {
                (RootKind::ByPk, col) => {
                    let Some(db_col) = ctx.column_db_name(col) else {
                        return Err(unexpected_arg(path, col));
                    };
                    if !ctx.info.primary_key.iter().any(|c| c == &db_col)
                        || !ctx.column_allowed(&db_col)
                    {
                        return Err(unexpected_arg(path, col));
                    }
                    let pg_type = ctx.info.column(&db_col).unwrap().pg_type.clone();
                    pk_predicate.push(BoolExp::Compare {
                        column: db_col,
                        pg_type,
                        op: CompareOp::Eq(Scalar::Json(value)),
                    });
                }
                (_, "where") => {
                    user_where = Some(self.parse_bool_exp(&value, ctx, session, false, path)?);
                }
                (_, "order_by") => {
                    order_by = self.parse_order_by(&value, ctx, session, path)?;
                }
                (_, "limit") => limit = Some(parse_non_negative(&value, path, "limit")?),
                (_, "offset") => offset = Some(parse_non_negative(&value, path, "offset")?),
                (_, "distinct_on") => {
                    distinct_on = parse_columns_arg(&value, ctx, path)?;
                }
                // Tracked-function / computed-field arguments are consumed
                // by the caller.
                (_, "args")
                    if matches!(
                        from,
                        FromSource::Function { .. } | FromSource::RowFunction { .. }
                    ) => {}
                (_, other) => return Err(unexpected_arg(path, other)),
            }
        }

        if kind == RootKind::ByPk {
            for pk in &ctx.info.primary_key {
                if !pk_predicate
                    .iter()
                    .any(|p| matches!(p, BoolExp::Compare { column, .. } if column == pk))
                {
                    return Err(PlanError::validation(
                        path,
                        format!(
                            "missing required field argument: \"{}\"",
                            ctx.column_graphql_name(pk)
                        ),
                    ));
                }
            }
        }

        // DISTINCT ON columns must lead ORDER BY.
        if !distinct_on.is_empty() {
            let mut prefix: Vec<OrderBy> = distinct_on
                .iter()
                .filter(|c| {
                    !order_by
                        .iter()
                        .any(|ob| matches!(&ob.target, OrderByTarget::Column(col) if col == *c))
                })
                .map(|c| OrderBy {
                    target: OrderByTarget::Column(c.clone()),
                    direction: OrderDirection::Asc,
                    nulls: NullsOrder::Last,
                })
                .collect();
            prefix.append(&mut order_by);
            order_by = prefix;
        }

        // Merge user where with the role's row filter.
        let perm_filter = self.permission_predicate(ctx, session, path)?;
        let mut predicates = pk_predicate;
        if let Some(w) = user_where {
            predicates.push(w);
        }
        if let Some(p) = perm_filter {
            predicates.push(p);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        // The permission limit caps what clients see: on plain selects it
        // merges into LIMIT; on aggregates it only caps `nodes` (the
        // aggregate itself is computed over the full filtered set).
        let mut nodes_limit = None;
        if kind != RootKind::ByPk {
            if let Some(perm_limit) = ctx.select_perm_limit() {
                if kind == RootKind::Aggregate {
                    nodes_limit = Some(perm_limit);
                } else {
                    limit = Some(limit.map_or(perm_limit, |l: u64| l.min(perm_limit)));
                }
            }
        }

        let fields = match kind {
            RootKind::Aggregate => self.walk_aggregate_selection(
                ctx,
                &field.selection_set,
                fragments,
                vars,
                session,
                path,
            )?,
            _ => self.walk_table_selection(
                ctx,
                &field.selection_set,
                fragments,
                vars,
                session,
                path,
            )?,
        };

        Ok(SelectQuery {
            from,
            fields,
            predicate,
            order_by,
            limit,
            nodes_limit,
            offset,
            distinct_on,
            single: kind == RootKind::ByPk,
        })
    }

    /// The role's row filter as an IR predicate (session vars substituted).
    /// Parsed in a permission-filter context: the filter may reference
    /// columns outside the role's column mask.
    pub(crate) fn permission_predicate(
        &self,
        ctx: &TableCtx,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        self.combined_filter(ctx, &ctx.perms, session, path)
    }

    /// OR of the given permissions' row filters; None when any of them is
    /// unrestricted (or the list is empty).
    pub(crate) fn combined_filter(
        &self,
        ctx: &TableCtx,
        perms: &[&SelectPermission],
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        if perms.is_empty() {
            return Ok(None);
        }
        let filter_ctx = TableCtx {
            entry: ctx.entry,
            info: ctx.info,
            perms: vec![],
            type_name: ctx.type_name.clone(),
        };
        let mut parts = vec![];
        for perm in perms {
            let filter = &perm.filter;
            if filter.is_null() || filter.as_object().is_some_and(|o| o.is_empty()) {
                return Ok(None);
            }
            parts.push(self.parse_bool_exp(filter, &filter_ctx, session, true, path)?);
        }
        Ok(Some(match parts.len() {
            1 => parts.pop().unwrap(),
            _ => BoolExp::Or(parts),
        }))
    }

    pub(crate) fn walk_table_selection(
        &self,
        ctx: &TableCtx,
        selection_set: &SelectionSet<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<Vec<OutputField>, PlanError> {
        let fields = flatten(selection_set, fragments, vars, Some(&ctx.type_name))?;
        if fields.is_empty() {
            return Err(PlanError::validation(
                path,
                format!("missing selection set for type '{}'", ctx.type_name),
            ));
        }

        let mut out = vec![];
        for field in fields {
            let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
            let fpath = format!("{path}.selectionSet.{}", field.name);

            if field.name == "__typename" {
                out.push(OutputField {
                    alias,
                    value: FieldValue::Typename {
                        value: ctx.type_name.clone(),
                    },
                });
                continue;
            }

            // Relay: `id` is the global object id; `<rel>_connection`
            // wraps an array relationship.
            if self.relay {
                if field.name == "id" {
                    out.push(OutputField {
                        alias,
                        value: FieldValue::RelayGlobalId {
                            schema: ctx.info.schema.clone(),
                            table: ctx.info.name.clone(),
                            pk: self.pk_typed(ctx),
                        },
                    });
                    continue;
                }
                if let Some(base) = field.name.strip_suffix("_connection") {
                    if let Some(rel) = ctx
                        .entry
                        .array_relationships
                        .iter()
                        .find(|r| r.name == base)
                    {
                        let (remote_table, join) = self.array_rel_target(ctx, rel, &fpath)?;
                        let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role)
                        else {
                            return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                        };
                        let conn = self.build_connection(
                            &remote, field, fragments, vars, session, &fpath, join,
                        )?;
                        out.push(OutputField {
                            alias,
                            value: FieldValue::NestedConnection {
                                conn: Box::new(conn),
                            },
                        });
                        continue;
                    }
                }
            }

            // Plain column?
            if let Some(db_name) = ctx
                .column_db_name(&field.name)
                .filter(|c| ctx.column_allowed(c))
            {
                if !field.arguments.is_empty() {
                    return Err(unexpected_arg(&fpath, &field.arguments[0].0));
                }
                let col = ctx.info.column(&db_name).unwrap();
                // Inherited roles: when only SOME parents grant the column,
                // it is NULLed on rows those parents cannot see.
                let granting = ctx.granting_perms(&db_name);
                let guard = if ctx.perms.len() > 1 && granting.len() < ctx.perms.len() {
                    self.combined_filter(ctx, &granting, session, &fpath)?
                } else {
                    None
                };
                out.push(OutputField {
                    alias,
                    value: match guard {
                        Some(guard) => FieldValue::ColumnGuarded {
                            column: col.name.clone(),
                            pg_type: col.pg_type.clone(),
                            guard,
                        },
                        None => FieldValue::Column {
                            column: col.name.clone(),
                            pg_type: col.pg_type.clone(),
                        },
                    },
                });
                continue;
            }

            // Object relationship?
            if let Some(rel) = ctx
                .entry
                .object_relationships
                .iter()
                .find(|r| r.name == field.name)
            {
                let (remote_table, join) = self.object_rel_target(ctx, rel, &fpath)?;
                let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role) else {
                    return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                };
                let fields = self.walk_table_selection(
                    &remote,
                    &field.selection_set,
                    fragments,
                    vars,
                    session,
                    &fpath,
                )?;
                let predicate = self.permission_predicate(&remote, session, &fpath)?;
                out.push(OutputField {
                    alias,
                    value: FieldValue::Object {
                        query: SelectQuery {
                            from: FromSource::Table(Table {
                                schema: remote.info.schema.clone(),
                                name: remote.info.name.clone(),
                            }),
                            fields,
                            predicate,
                            order_by: vec![],
                            limit: None,
                            nodes_limit: None,
                            offset: None,
                            distinct_on: vec![],
                            single: true,
                        },
                        join,
                    },
                });
                continue;
            }

            // Array relationship (plain or _aggregate)?
            let (rel_name, aggregate) = match field.name.strip_suffix("_aggregate") {
                Some(base) if ctx.entry.array_relationships.iter().any(|r| r.name == base) => {
                    (base.to_string(), true)
                }
                _ => (field.name.clone(), false),
            };
            if let Some(rel) = ctx
                .entry
                .array_relationships
                .iter()
                .find(|r| r.name == rel_name)
            {
                let (remote_table, join) = self.array_rel_target(ctx, rel, &fpath)?;
                let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role) else {
                    return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                };
                if aggregate && !remote.allow_aggregations() {
                    return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                }
                let kind = if aggregate {
                    RootKind::Aggregate
                } else {
                    RootKind::Select
                };
                let from = FromSource::Table(Table {
                    schema: remote.info.schema.clone(),
                    name: remote.info.name.clone(),
                });
                let query = self
                    .build_select(&remote, kind, from, field, fragments, vars, session, &fpath)?;
                out.push(OutputField {
                    alias,
                    value: FieldValue::Array {
                        query,
                        join,
                        aggregate,
                    },
                });
                continue;
            }

            // Remote relationship? (visible only when the role has a
            // permission on the target remote schema)
            if let Some(rel) = ctx
                .entry
                .remote_relationships
                .iter()
                .find(|r| r.name == field.name)
            {
                let permitted = self.remote_schemas.iter().any(|s| {
                    s.name == rel.remote_schema
                        && s.permissions.iter().any(|p| p.role == session.role)
                });
                if !permitted {
                    return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                }
                // Remote-relationship fields take no client arguments.
                if let Some((arg, _)) = field.arguments.first() {
                    return Err(PlanError::validation(
                        &fpath,
                        format!("'{}' has no argument named '{arg}'", field.name),
                    ));
                }
                // Hidden columns carry the joining values.
                for col in &rel.donat_fields {
                    let hidden = format!("__rr__{col}");
                    if !out.iter().any(|f: &OutputField| f.alias == hidden) {
                        if let Some(info) = ctx.info.column(col) {
                            out.push(OutputField {
                                alias: hidden,
                                value: FieldValue::Column {
                                    column: info.name.clone(),
                                    pg_type: info.pg_type.clone(),
                                },
                            });
                        }
                    }
                }
                // Build `query($v0: T!) { <root>(arg: $v0, lit: x) {sel} }`.
                let Some((root_field, spec)) =
                    rel.remote_field.as_object().and_then(|m| m.iter().next())
                else {
                    return Err(PlanError::validation(&fpath, "invalid remote_field"));
                };
                let mut var_defs = vec![];
                let mut args_sdl = vec![];
                let mut variables = vec![];
                if let Some(arguments) = spec.get("arguments").and_then(|a| a.as_object()) {
                    for (arg, value) in arguments {
                        let rendered = render_remote_arg(value, ctx, &mut var_defs, &mut variables);
                        args_sdl.push(format!("{arg}: {rendered}"));
                    }
                }
                let selection = render_selection(&field.selection_set, fragments, vars)?;
                let args_part = if args_sdl.is_empty() {
                    String::new()
                } else {
                    format!("({})", args_sdl.join(", "))
                };
                let var_part = if var_defs.is_empty() {
                    String::new()
                } else {
                    format!("({})", var_defs.join(", "))
                };
                let query = format!("query{var_part} {{ {root_field}{args_part} {selection} }}");
                out.push(OutputField {
                    alias,
                    value: FieldValue::RemoteJoin {
                        spec: RemoteJoinSpec {
                            schema: rel.remote_schema.clone(),
                            query,
                            variables,
                            root_field: root_field.clone(),
                        },
                    },
                });
                continue;
            }

            // Computed field?
            if let Some(cf) = ctx
                .entry
                .computed_fields
                .iter()
                .find(|c| c.name == field.name)
            {
                let def = &cf.definition;
                let Some(finfo) = self
                    .catalog
                    .function(def.function.schema(), def.function.name())
                else {
                    return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                };
                let user_args = field
                    .arguments
                    .iter()
                    .find(|(name, _)| name == "args")
                    .map(|(_, v)| value_to_json(v, vars, &fpath))
                    .transpose()?
                    .and_then(|v| v.as_object().cloned());
                let args =
                    self.computed_field_args(finfo, def, session, user_args.as_ref(), &fpath)?;

                if let Some((rschema, rname)) = &finfo.returns_table {
                    // Table-valued computed field: behaves like an array
                    // relationship, governed by the remote table's perms.
                    let remote_table = QualifiedTable::Qualified {
                        schema: rschema.clone(),
                        name: rname.clone(),
                    };
                    let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role) else {
                        return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                    };
                    let from = FromSource::RowFunction {
                        schema: finfo.schema.clone(),
                        name: finfo.name.clone(),
                        args,
                    };
                    let query = self.build_select(
                        &remote,
                        RootKind::Select,
                        from,
                        field,
                        fragments,
                        vars,
                        session,
                        &fpath,
                    )?;
                    out.push(OutputField {
                        alias,
                        value: FieldValue::Array {
                            query,
                            join: vec![],
                            aggregate: false,
                        },
                    });
                } else {
                    // Scalar computed field: must be granted in the select
                    // permission's computed_fields list.
                    if !ctx.computed_field_allowed(&field.name) {
                        return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
                    }
                    let granting: Vec<&SelectPermission> = ctx
                        .perms
                        .iter()
                        .copied()
                        .filter(|p| p.computed_fields.iter().any(|c| c == &field.name))
                        .collect();
                    let guard = if ctx.perms.len() > 1 && granting.len() < ctx.perms.len() {
                        self.combined_filter(ctx, &granting, session, &fpath)?
                            .map(Box::new)
                    } else {
                        None
                    };
                    out.push(OutputField {
                        alias,
                        value: FieldValue::ComputedScalar {
                            schema: finfo.schema.clone(),
                            name: finfo.name.clone(),
                            args,
                            guard,
                        },
                    });
                }
                continue;
            }

            return Err(field_not_found(&fpath, &field.name, &ctx.type_name));
        }
        Ok(out)
    }

    fn walk_aggregate_selection(
        &self,
        ctx: &TableCtx,
        selection_set: &SelectionSet<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<Vec<OutputField>, PlanError> {
        let agg_type_name = format!("{}_aggregate", ctx.type_name);
        let fields = flatten(selection_set, fragments, vars, Some(&agg_type_name))?;
        let mut out = vec![];
        for field in fields {
            let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
            let fpath = format!("{path}.selectionSet.{}", field.name);
            match field.name.as_str() {
                "__typename" => out.push(OutputField {
                    alias,
                    value: FieldValue::Typename {
                        value: agg_type_name.clone(),
                    },
                }),
                "aggregate" => {
                    let aggs = self.parse_aggregate_fields(
                        ctx,
                        &field.selection_set,
                        fragments,
                        vars,
                        session,
                        &fpath,
                    )?;
                    out.push(OutputField {
                        alias,
                        value: FieldValue::Aggregate { fields: aggs },
                    });
                }
                "nodes" => {
                    let nodes = self.walk_table_selection(
                        ctx,
                        &field.selection_set,
                        fragments,
                        vars,
                        session,
                        &fpath,
                    )?;
                    out.push(OutputField {
                        alias,
                        value: FieldValue::Nodes { fields: nodes },
                    });
                }
                other => return Err(field_not_found(&fpath, other, &agg_type_name)),
            }
        }
        Ok(out)
    }

    fn parse_aggregate_fields(
        &self,
        ctx: &TableCtx<'a>,
        selection_set: &SelectionSet<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<Vec<AggregateField>, PlanError> {
        let fields = flatten(selection_set, fragments, vars, None)?;
        let mut out = vec![];
        for field in fields {
            let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
            let fpath = format!("{path}.selectionSet.{}", field.name);
            match field.name.as_str() {
                "count" => {
                    let mut distinct = false;
                    let mut columns = vec![];
                    for (arg, value) in &field.arguments {
                        let value = value_to_json(value, vars, &fpath)?;
                        match arg.as_str() {
                            "distinct" => distinct = value.as_bool().unwrap_or(false),
                            "columns" => {
                                // Donat omits the columns arg from count for
                                // roles with no selectable columns.
                                if !ctx.any_column_allowed() {
                                    return Err(PlanError::validation(
                                        &fpath,
                                        "'count' has no argument named 'columns'",
                                    ));
                                }
                                columns = parse_columns_arg(&value, ctx, &fpath)?;
                            }
                            other => return Err(unexpected_arg(&fpath, other)),
                        }
                    }
                    out.push(AggregateField {
                        alias,
                        op: AggregateOp::Count { distinct, columns },
                    });
                }
                op if AGGREGATE_COLUMN_OPS.contains(&op) => {
                    // Roles with no selectable columns get no column-op
                    // fields in <table>_aggregate_fields at all.
                    if !ctx.any_column_allowed() {
                        return Err(field_not_found(
                            &fpath,
                            op,
                            &format!("{}_aggregate_fields", ctx.type_name),
                        ));
                    }
                    let cols = flatten(&field.selection_set, fragments, vars, None)?;
                    let mut columns = vec![];
                    for col_field in cols {
                        let col_alias = col_field
                            .alias
                            .clone()
                            .unwrap_or_else(|| col_field.name.clone());
                        let Some(db_name) = ctx
                            .column_db_name(&col_field.name)
                            .filter(|c| ctx.column_allowed(c))
                        else {
                            return Err(field_not_found(&fpath, &col_field.name, &ctx.type_name));
                        };
                        let info = ctx.info.column(&db_name).unwrap();
                        let granting = ctx.granting_perms(&db_name);
                        let guard = if ctx.perms.len() > 1 && granting.len() < ctx.perms.len() {
                            self.combined_filter(ctx, &granting, session, &fpath)?
                        } else {
                            None
                        };
                        columns.push(AggregateColumn {
                            alias: col_alias,
                            column: info.name.clone(),
                            pg_type: info.pg_type.clone(),
                            guard,
                        });
                    }
                    out.push(AggregateField {
                        alias,
                        op: AggregateOp::ColumnOp {
                            op: op.to_string(),
                            columns,
                        },
                    });
                }
                other => {
                    return Err(field_not_found(&fpath, other, "aggregate fields"));
                }
            }
        }
        Ok(out)
    }

    fn parse_order_by(
        &self,
        value: &Json,
        ctx: &TableCtx,
        session: &Session,
        path: &str,
    ) -> Result<Vec<OrderBy>, PlanError> {
        let items: Vec<&Json> = match value {
            Json::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        let mut out = vec![];
        for item in items {
            let Json::Object(map) = item else {
                return Err(PlanError::validation(path, "expected an order_by object"));
            };
            for (key, dir_value) in map {
                if let Some(db_key) = ctx.column_db_name(key).filter(|c| ctx.column_allowed(c)) {
                    let (direction, nulls) = parse_order_direction(dir_value, path)?;
                    out.push(OrderBy {
                        target: OrderByTarget::Column(db_key),
                        direction,
                        nulls,
                    });
                    continue;
                }

                // Aggregate over an array relationship?
                if let Some(base) = key.strip_suffix("_aggregate") {
                    if let Some(rel) = ctx
                        .entry
                        .array_relationships
                        .iter()
                        .find(|r| r.name == base)
                    {
                        let (remote_table, join) = self.array_rel_target(ctx, rel, path)?;
                        let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role)
                        else {
                            return Err(field_not_found(path, key, &ctx.type_name));
                        };
                        let predicate = self
                            .permission_predicate(&remote, session, path)?
                            .map(Box::new);
                        let Json::Object(inner) = dir_value else {
                            return Err(PlanError::validation(path, "expected an order_by object"));
                        };
                        let remote_ir_table = Table {
                            schema: remote.info.schema.clone(),
                            name: remote.info.name.clone(),
                        };
                        for (agg, agg_value) in inner {
                            if agg == "count" {
                                let (direction, nulls) = parse_order_direction(agg_value, path)?;
                                out.push(OrderBy {
                                    target: OrderByTarget::RelationshipAggregate {
                                        table: remote_ir_table.clone(),
                                        join: join.clone(),
                                        function: "count".into(),
                                        column: None,
                                        predicate: predicate.clone(),
                                    },
                                    direction,
                                    nulls,
                                });
                            } else {
                                // The function name is rendered into SQL
                                // verbatim by sqlgen, so it must be one of the
                                // fixed column-aggregate ops — never arbitrary
                                // input.
                                if !AGGREGATE_COLUMN_OPS.contains(&agg.as_str()) {
                                    return Err(field_not_found(
                                        path,
                                        agg,
                                        &format!("{}_aggregate_order_by", remote.type_name),
                                    ));
                                }
                                let Json::Object(cols) = agg_value else {
                                    return Err(PlanError::validation(
                                        path,
                                        "expected an order_by object",
                                    ));
                                };
                                for (col, dir_value) in cols {
                                    let Some(db_col) = remote
                                        .column_db_name(col)
                                        .filter(|c| remote.column_allowed(c))
                                    else {
                                        return Err(field_not_found(path, col, &remote.type_name));
                                    };
                                    let (direction, nulls) =
                                        parse_order_direction(dir_value, path)?;
                                    out.push(OrderBy {
                                        target: OrderByTarget::RelationshipAggregate {
                                            table: remote_ir_table.clone(),
                                            join: join.clone(),
                                            function: agg.clone(),
                                            column: Some(db_col),
                                            predicate: predicate.clone(),
                                        },
                                        direction,
                                        nulls,
                                    });
                                }
                            }
                        }
                        continue;
                    }
                }

                if let Some(rel) = ctx
                    .entry
                    .object_relationships
                    .iter()
                    .find(|r| r.name == *key)
                {
                    let (remote_table, join) = self.object_rel_target(ctx, rel, path)?;
                    let Some(remote) = self.table_ctx_by_name(&remote_table, &session.role) else {
                        return Err(field_not_found(path, key, &ctx.type_name));
                    };
                    let predicate = self
                        .permission_predicate(&remote, session, path)?
                        .map(Box::new);
                    let Json::Object(inner) = dir_value else {
                        return Err(PlanError::validation(path, "expected an order_by object"));
                    };
                    for (col, dir_value) in inner {
                        let Some(db_col) = remote
                            .column_db_name(col)
                            .filter(|c| remote.column_allowed(c))
                        else {
                            return Err(field_not_found(path, col, &remote.type_name));
                        };
                        let (direction, nulls) = parse_order_direction(dir_value, path)?;
                        out.push(OrderBy {
                            target: OrderByTarget::Relationship {
                                table: Table {
                                    schema: remote.info.schema.clone(),
                                    name: remote.info.name.clone(),
                                },
                                join: join.clone(),
                                column: db_col,
                                predicate: predicate.clone(),
                            },
                            direction,
                            nulls,
                        });
                    }
                } else {
                    return Err(field_not_found(path, key, &ctx.type_name));
                }
            }
        }
        Ok(out)
    }

    /// Resolve an object relationship to (remote table, join pairs).
    fn object_rel_target(
        &self,
        ctx: &TableCtx,
        rel: &donat_metadata::ObjectRelationship,
        path: &str,
    ) -> Result<(QualifiedTable, Vec<(String, String)>), PlanError> {
        if let Some(manual) = &rel.using.manual_configuration {
            let join = manual
                .column_mapping
                .iter()
                .map(|(l, r)| (l.clone(), r.clone()))
                .collect();
            return Ok((manual.remote_table.clone(), join));
        }
        if let Some(fk_cols) = &rel.using.foreign_key_constraint_on {
            let cols: Vec<String> = match fk_cols {
                donat_metadata::ObjRelFkColumns::Single(c) => vec![c.clone()],
                donat_metadata::ObjRelFkColumns::Multiple(cs) => cs.clone(),
                donat_metadata::ObjRelFkColumns::Remote(remote) => {
                    return self.remote_fk_object_rel_target(ctx, remote, rel, path);
                }
            };
            let fk = ctx
                .info
                .foreign_keys
                .iter()
                .find(|fk| {
                    fk.column_mapping.len() == cols.len()
                        && cols.iter().all(|c| fk.column_mapping.contains_key(c))
                })
                .ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!(
                            "no foreign key constraint on ({}) for relationship '{}'",
                            cols.join(", "),
                            rel.name
                        ),
                    )
                })?;
            let join = fk
                .column_mapping
                .iter()
                .map(|(l, r)| (l.clone(), r.clone()))
                .collect();
            return Ok((
                QualifiedTable::Qualified {
                    schema: fk.referenced_schema.clone(),
                    name: fk.referenced_table.clone(),
                },
                join,
            ));
        }
        Err(PlanError::validation(
            path,
            format!("relationship '{}' has no using clause", rel.name),
        ))
    }

    fn remote_fk_object_rel_target(
        &self,
        ctx: &TableCtx,
        remote: &donat_metadata::ArrRelFkConstraint,
        rel: &donat_metadata::ObjectRelationship,
        path: &str,
    ) -> Result<(QualifiedTable, Vec<(String, String)>), PlanError> {
        let remote_table = remote.table.clone();
        let mut fk_cols: Vec<String> = vec![];
        if let Some(c) = &remote.column {
            fk_cols.push(c.clone());
        }
        if let Some(cs) = &remote.columns {
            fk_cols.extend(cs.iter().cloned());
        }
        let remote_info = self
            .catalog
            .table(remote_table.schema(), remote_table.name())
            .ok_or_else(|| {
                PlanError::validation(
                    path,
                    format!(
                        "table '{remote_table}' not found for relationship '{}'",
                        rel.name
                    ),
                )
            })?;
        let fk = remote_info
            .foreign_keys
            .iter()
            .find(|fk| {
                fk.referenced_schema == ctx.entry.table.schema()
                    && fk.referenced_table == ctx.entry.table.name()
                    && fk.column_mapping.len() == fk_cols.len()
                    && fk_cols.iter().all(|c| fk.column_mapping.contains_key(c))
            })
            .ok_or_else(|| {
                PlanError::validation(
                    path,
                    format!(
                        "no foreign key from '{remote_table}' on ({}) to '{}' for relationship '{}'",
                        fk_cols.join(", "),
                        ctx.entry.table,
                        rel.name
                    ),
                )
            })?;
        let join = fk
            .column_mapping
            .iter()
            .map(|(remote_col, local_col)| (local_col.clone(), remote_col.clone()))
            .collect();
        Ok((remote_table, join))
    }

    /// Resolve an array relationship to (remote table, join pairs);
    /// join pairs are (local column, remote column).
    fn array_rel_target(
        &self,
        ctx: &TableCtx,
        rel: &donat_metadata::ArrayRelationship,
        path: &str,
    ) -> Result<(QualifiedTable, Vec<(String, String)>), PlanError> {
        if let Some(manual) = &rel.using.manual_configuration {
            let join = manual
                .column_mapping
                .iter()
                .map(|(l, r)| (l.clone(), r.clone()))
                .collect();
            return Ok((manual.remote_table.clone(), join));
        }
        if let Some(fk) = &rel.using.foreign_key_constraint_on {
            let remote_table = fk.table.clone();
            let mut fk_cols: Vec<String> = vec![];
            if let Some(c) = &fk.column {
                fk_cols.push(c.clone());
            }
            if let Some(cs) = &fk.columns {
                fk_cols.extend(cs.iter().cloned());
            }
            let remote_info = self
                .catalog
                .table(remote_table.schema(), remote_table.name())
                .ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!(
                            "table '{remote_table}' not found for relationship '{}'",
                            rel.name
                        ),
                    )
                })?;
            let constraint = remote_info
                .foreign_keys
                .iter()
                .find(|c| {
                    c.referenced_schema == ctx.info.schema
                        && c.referenced_table == ctx.info.name
                        && c.column_mapping.len() == fk_cols.len()
                        && fk_cols.iter().all(|col| c.column_mapping.contains_key(col))
                })
                .ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!(
                            "no foreign key constraint on {remote_table}({}) for relationship '{}'",
                            fk_cols.join(", "),
                            rel.name
                        ),
                    )
                })?;
            // FK lives on the remote table: remote col -> our col.
            let join = constraint
                .column_mapping
                .iter()
                .map(|(remote_col, our_col)| (our_col.clone(), remote_col.clone()))
                .collect();
            return Ok((remote_table, join));
        }
        Err(PlanError::validation(
            path,
            format!("relationship '{}' has no using clause", rel.name),
        ))
    }
}

/// Render a remote-relationship argument value as a GraphQL literal,
/// turning "$column" strings into typed variables bound to hidden row
/// columns.
fn render_remote_arg(
    value: &Json,
    ctx: &TableCtx,
    var_defs: &mut Vec<String>,
    variables: &mut Vec<(String, String)>,
) -> String {
    match value {
        Json::String(s) if s.starts_with('$') => {
            let col = s.trim_start_matches('$');
            let pg_type = ctx
                .info
                .column(col)
                .map(|c| c.pg_type.as_str())
                .unwrap_or("text");
            let gql_type = match pg_type {
                "int2" | "int4" | "int8" => "Int!",
                "bool" => "Boolean!",
                _ => "String!",
            };
            let var = format!("v{}", variables.len());
            var_defs.push(format!("${var}: {gql_type}"));
            variables.push((var.clone(), format!("__rr__{col}")));
            format!("${var}")
        }
        Json::String(s) => format!("{:?}", s),
        Json::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", render_remote_arg(v, ctx, var_defs, variables)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Json::Array(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|v| render_remote_arg(v, ctx, var_defs, variables))
                .collect();
            format!("[{}]", inner.join(", "))
        }
        other => other.to_string(),
    }
}

/// Render a selection set back to GraphQL text (fragments expanded).
pub(crate) fn render_selection(
    set: &SelectionSet<'static, String>,
    fragments: &Fragments,
    vars: &JsonMap<String, Json>,
) -> Result<String, PlanError> {
    let fields = flatten(set, fragments, vars, None)?;
    let mut parts = vec![];
    for f in fields {
        let alias = match &f.alias {
            Some(a) => format!("{a}: "),
            None => String::new(),
        };
        let args = if f.arguments.is_empty() {
            String::new()
        } else {
            let rendered: Vec<String> = f
                .arguments
                .iter()
                .map(|(n, v)| format!("{n}: {v}"))
                .collect();
            format!("({})", rendered.join(", "))
        };
        let sub = if f.selection_set.items.is_empty() {
            String::new()
        } else {
            format!(" {}", render_selection(&f.selection_set, fragments, vars)?)
        };
        parts.push(format!("{alias}{}{args}{sub}", f.name));
    }
    Ok(format!("{{ {} }}", parts.join(" ")))
}

/// All session variables as the json object Donat passes to
/// session-aware SQL functions.
pub(crate) fn session_json(session: &Session) -> String {
    let map: JsonMap<String, Json> = session
        .vars
        .iter()
        .map(|(k, v)| (k.clone(), Json::String(v.clone())))
        .collect();
    Json::Object(map).to_string()
}

impl<'a> Planner<'a> {
    fn pk_typed(&self, ctx: &TableCtx<'a>) -> Vec<(String, String)> {
        ctx.info
            .primary_key
            .iter()
            .filter_map(|c| {
                ctx.info
                    .column(c)
                    .map(|info| (c.clone(), info.pg_type.clone()))
            })
            .collect()
    }

    /// Relay roots: `<base>_connection` and `node(id:)`.
    fn plan_relay_root(
        &self,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<Option<RootField>, PlanError> {
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());

        if field.name == "node" {
            let id_value = field
                .arguments
                .iter()
                .find(|(n, _)| n == "id")
                .map(|(_, v)| value_to_json(v, vars, path))
                .transpose()?
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| PlanError::validation(path, "expecting an id"))?;
            use base64::Engine as _;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(id_value.as_bytes())
                .ok()
                .and_then(|b| serde_json::from_slice::<Json>(&b).ok())
                .and_then(|v| v.as_array().cloned())
                .ok_or_else(|| PlanError::validation(path, "invalid node id"))?;
            if decoded.len() < 4 {
                return Err(PlanError::validation(path, "invalid node id"));
            }
            let schema = decoded[1].as_str().unwrap_or_default().to_string();
            let table_name = decoded[2].as_str().unwrap_or_default().to_string();
            let table = QualifiedTable::Qualified {
                schema: schema.clone(),
                name: table_name.clone(),
            };
            let Some(ctx) = self.table_ctx_by_name(&table, &session.role) else {
                return Ok(Some(RootField::Select {
                    alias,
                    query: SelectQuery {
                        from: FromSource::Table(Table {
                            schema: "pg_catalog".into(),
                            name: "pg_class".into(),
                        }),
                        fields: vec![],
                        predicate: Some(BoolExp::Or(vec![])),
                        order_by: vec![],
                        limit: Some(0),
                        nodes_limit: None,
                        offset: None,
                        distinct_on: vec![],
                        single: true,
                    },
                }));
            };
            let mut predicates = vec![];
            for (i, (col, pg_type)) in self.pk_typed(&ctx).iter().enumerate() {
                let value = decoded.get(3 + i).cloned().unwrap_or(Json::Null);
                predicates.push(BoolExp::Compare {
                    column: col.clone(),
                    pg_type: pg_type.clone(),
                    op: CompareOp::Eq(Scalar::Json(value)),
                });
            }
            if let Some(perm) = self.permission_predicate(&ctx, session, path)? {
                predicates.push(perm);
            }
            let fields = self.walk_table_selection(
                &ctx,
                &field.selection_set,
                fragments,
                vars,
                session,
                path,
            )?;
            return Ok(Some(RootField::Select {
                alias,
                query: SelectQuery {
                    from: FromSource::Table(Table {
                        schema: ctx.info.schema.clone(),
                        name: ctx.info.name.clone(),
                    }),
                    fields,
                    predicate: Some(BoolExp::And(predicates)),
                    order_by: vec![],
                    limit: None,
                    nodes_limit: None,
                    offset: None,
                    distinct_on: vec![],
                    single: true,
                },
            }));
        }

        let Some(base) = field.name.strip_suffix("_connection") else {
            return Ok(None);
        };
        // Resolve the base name through the normal select roots.
        let Some(&(RootKind::Select, RootSource::Table(idx))) = self.roots.get(base) else {
            return Ok(None);
        };
        let Some(ctx) = self.table_ctx(idx, &session.role) else {
            return Ok(None);
        };
        let conn = self.build_connection(&ctx, field, fragments, vars, session, path, vec![])?;
        Ok(Some(RootField::Connection { alias, conn }))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_connection(
        &self,
        ctx: &TableCtx<'a>,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
        join: Vec<(String, String)>,
    ) -> Result<Connection, PlanError> {
        // Connection-level selection: pageInfo / edges / __typename.
        let mut conn_fields = vec![];
        let mut node_fields: Vec<OutputField> = vec![];
        let conn_type = format!("{}Connection", ctx.type_name);
        for sub in flatten(&field.selection_set, fragments, vars, None)? {
            let sub_alias = sub.alias.clone().unwrap_or_else(|| sub.name.clone());
            match sub.name.as_str() {
                "__typename" => conn_fields.push(ConnectionField::Typename {
                    alias: sub_alias,
                    value: conn_type.clone(),
                }),
                "pageInfo" => {
                    let mut infos = vec![];
                    for pf in flatten(&sub.selection_set, fragments, vars, None)? {
                        let pa = pf.alias.clone().unwrap_or_else(|| pf.name.clone());
                        infos.push((pa, pf.name.clone()));
                    }
                    conn_fields.push(ConnectionField::PageInfo {
                        alias: sub_alias,
                        fields: infos,
                    });
                }
                "edges" => {
                    let mut edge_fields = vec![];
                    for ef in flatten(&sub.selection_set, fragments, vars, None)? {
                        let ea = ef.alias.clone().unwrap_or_else(|| ef.name.clone());
                        match ef.name.as_str() {
                            "cursor" => edge_fields.push(EdgeField::Cursor { alias: ea }),
                            "node" => {
                                node_fields = self.walk_table_selection(
                                    ctx,
                                    &ef.selection_set,
                                    fragments,
                                    vars,
                                    session,
                                    path,
                                )?;
                                edge_fields.push(EdgeField::Node { alias: ea });
                            }
                            "__typename" => edge_fields.push(EdgeField::Typename {
                                alias: ea,
                                value: format!("{}Edge", ctx.type_name),
                            }),
                            other => {
                                return Err(field_not_found(path, other, "edges"));
                            }
                        }
                    }
                    conn_fields.push(ConnectionField::Edges {
                        alias: sub_alias,
                        fields: edge_fields,
                    });
                }
                other => return Err(field_not_found(path, other, &conn_type)),
            }
        }

        // Row-source arguments (where/order_by/...); relay pagination args
        // (first/after/last/before) are not supported yet.
        let from = FromSource::Table(Table {
            schema: ctx.info.schema.clone(),
            name: ctx.info.name.clone(),
        });
        let mut user_where = None;
        let mut order_by = vec![];
        let mut limit = None;
        let mut first = None;
        let mut last = None;
        let mut after = None;
        let mut before = None;
        for (arg_name, arg_value) in &field.arguments {
            let value = value_to_json(arg_value, vars, path)?;
            match arg_name.as_str() {
                "where" => {
                    user_where = Some(self.parse_bool_exp(&value, ctx, session, false, path)?);
                }
                "order_by" => order_by = self.parse_order_by(&value, ctx, session, path)?,
                "first" => first = value.as_u64(),
                "last" => last = value.as_u64(),
                "after" => after = value.as_str().map(str::to_string),
                "before" => before = value.as_str().map(str::to_string),
                other => return Err(unexpected_arg(path, other)),
            }
        }
        let mut predicates = vec![];
        if let Some(w) = user_where {
            predicates.push(w);
        }
        // Cursor predicates: pk beyond the decoded cursor value.
        let decode_cursor = |raw: &str| -> Result<Json, PlanError> {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(raw.as_bytes())
                .ok()
                .and_then(|b| serde_json::from_slice::<Json>(&b).ok())
                .ok_or_else(|| PlanError::validation(path, "invalid cursor"))
        };
        let pk = self.pk_typed(ctx);
        if let Some(raw) = &after {
            let cursor = decode_cursor(raw)?;
            for (col, pg_type) in &pk {
                if let Some(v) = cursor.get(col) {
                    predicates.push(BoolExp::Compare {
                        column: col.clone(),
                        pg_type: pg_type.clone(),
                        op: CompareOp::Gt(Scalar::Json(v.clone())),
                    });
                }
            }
        }
        if let Some(raw) = &before {
            let cursor = decode_cursor(raw)?;
            for (col, pg_type) in &pk {
                if let Some(v) = cursor.get(col) {
                    predicates.push(BoolExp::Compare {
                        column: col.clone(),
                        pg_type: pg_type.clone(),
                        op: CompareOp::Lt(Scalar::Json(v.clone())),
                    });
                }
            }
        }
        let page = match (first, last) {
            (Some(size), _) => Some(RelayPage {
                size,
                backward: false,
                has_other_side: after.is_some(),
            }),
            (None, Some(size)) => Some(RelayPage {
                size,
                backward: true,
                has_other_side: before.is_some(),
            }),
            _ => None,
        };
        if let Some(p) = self.permission_predicate(ctx, session, path)? {
            predicates.push(p);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };
        if let Some(perm_limit) = ctx.select_perm_limit() {
            limit = Some(limit.map_or(perm_limit, |l: u64| l.min(perm_limit)));
        }

        Ok(Connection {
            query: SelectQuery {
                from,
                fields: node_fields,
                predicate,
                order_by,
                limit,
                nodes_limit: None,
                offset: None,
                distinct_on: vec![],
                single: false,
            },
            join,
            pk,
            schema: ctx.info.schema.clone(),
            table: ctx.info.name.clone(),
            fields: conn_fields,
            page,
        })
    }
}

fn op_name<'o>(op: &'o OperationDefinition<'static, String>) -> Option<&'o str> {
    match op {
        OperationDefinition::Query(q) => q.name.as_deref(),
        OperationDefinition::Mutation(m) => m.name.as_deref(),
        OperationDefinition::Subscription(s) => s.name.as_deref(),
        OperationDefinition::SelectionSet(_) => None,
    }
}

/// The column-aggregate functions (i.e. every aggregate except `count`).
/// Single source of truth for both `_aggregate_fields` selection and
/// `order_by` over a relationship aggregate — the function name reaches SQL
/// verbatim, so it must never be taken from unvalidated input.
pub(crate) const AGGREGATE_COLUMN_OPS: &[&str] = &[
    "sum",
    "avg",
    "max",
    "min",
    "stddev",
    "stddev_samp",
    "stddev_pop",
    "variance",
    "var_samp",
    "var_pop",
];

pub(crate) fn field_not_found(path: &str, field: &str, type_name: &str) -> PlanError {
    PlanError::validation(
        path,
        format!("field '{field}' not found in type: '{type_name}'"),
    )
}

pub(crate) fn unexpected_arg(path: &str, arg: &str) -> PlanError {
    PlanError::validation(path, format!("unexpected argument: \"{arg}\""))
}

pub(crate) fn parse_non_negative(value: &Json, path: &str, what: &str) -> Result<u64, PlanError> {
    value.as_u64().ok_or_else(|| {
        PlanError::validation(path, format!("expects a non-negative integer for {what}"))
    })
}

/// `distinct_on` / `count(columns:)`: single enum or list of enums.
fn parse_columns_arg(value: &Json, ctx: &TableCtx, path: &str) -> Result<Vec<String>, PlanError> {
    let raw: Vec<&Json> = match value {
        Json::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut out = vec![];
    for item in raw {
        let Some(name) = item.as_str() else {
            return Err(PlanError::validation(path, "expected a column name"));
        };
        let Some(db_name) = ctx.column_db_name(name).filter(|c| ctx.column_allowed(c)) else {
            return Err(field_not_found(path, name, &ctx.type_name));
        };
        out.push(db_name);
    }
    Ok(out)
}

fn parse_order_direction(
    value: &Json,
    path: &str,
) -> Result<(OrderDirection, NullsOrder), PlanError> {
    let Some(s) = value.as_str() else {
        return Err(PlanError::validation(
            path,
            "expected an order_by direction",
        ));
    };
    let res = match s {
        "asc" => (OrderDirection::Asc, NullsOrder::Last),
        "asc_nulls_first" => (OrderDirection::Asc, NullsOrder::First),
        "asc_nulls_last" => (OrderDirection::Asc, NullsOrder::Last),
        "desc" => (OrderDirection::Desc, NullsOrder::First),
        "desc_nulls_first" => (OrderDirection::Desc, NullsOrder::First),
        "desc_nulls_last" => (OrderDirection::Desc, NullsOrder::Last),
        other => {
            return Err(PlanError::validation(
                path,
                format!("unexpected value \"{other}\" for enum: 'order_by'"),
            ));
        }
    };
    Ok(res)
}

/// Resolve a GraphQL value to JSON, substituting variables.
pub(crate) fn value_to_json(
    value: &GqlValue<'static, String>,
    vars: &JsonMap<String, Json>,
    path: &str,
) -> Result<Json, PlanError> {
    Ok(match value {
        GqlValue::Variable(name) => vars.get(name).cloned().ok_or_else(|| {
            PlanError::validation(
                path,
                format!("expecting a value for non-nullable variable: \"{name}\""),
            )
        })?,
        GqlValue::Int(n) => Json::from(n.as_i64().unwrap_or_default()),
        GqlValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        GqlValue::String(s) => Json::String(s.clone()),
        GqlValue::Boolean(b) => Json::Bool(*b),
        GqlValue::Null => Json::Null,
        GqlValue::Enum(e) => Json::String(e.clone()),
        GqlValue::List(items) => Json::Array(
            items
                .iter()
                .map(|v| value_to_json(v, vars, path))
                .collect::<Result<_, _>>()?,
        ),
        GqlValue::Object(map) => {
            let mut out = JsonMap::new();
            for (k, v) in map {
                out.insert(k.clone(), value_to_json(v, vars, path)?);
            }
            Json::Object(out)
        }
    })
}

/// Flatten a selection set: resolve fragment spreads and inline fragments,
/// apply @include/@skip. `type_name` checks fragment type conditions when
/// known.
pub(crate) fn flatten<'s>(
    selection_set: &'s SelectionSet<'static, String>,
    fragments: &Fragments<'s>,
    vars: &JsonMap<String, Json>,
    type_name: Option<&str>,
) -> Result<Vec<&'s GqlField<'static, String>>, PlanError> {
    let mut out = vec![];
    flatten_into(selection_set, fragments, vars, type_name, &mut out)?;
    Ok(out)
}

pub(crate) fn flatten_into<'s>(
    selection_set: &'s SelectionSet<'static, String>,
    fragments: &Fragments<'s>,
    vars: &JsonMap<String, Json>,
    type_name: Option<&str>,
    out: &mut Vec<&'s GqlField<'static, String>>,
) -> Result<(), PlanError> {
    for selection in &selection_set.items {
        match selection {
            Selection::Field(f) => {
                if directives_include(&f.directives, vars)? {
                    out.push(f);
                }
            }
            Selection::FragmentSpread(spread) => {
                if !directives_include(&spread.directives, vars)? {
                    continue;
                }
                let fragment = fragments.get(&spread.fragment_name).ok_or_else(|| {
                    PlanError::validation(
                        "$",
                        format!("fragment \"{}\" not found", spread.fragment_name),
                    )
                })?;
                let graphql_parser::query::TypeCondition::On(on) = &fragment.type_condition;
                if let Some(tn) = type_name {
                    if on != tn {
                        return Err(PlanError::validation(
                            "$",
                            format!(
                                "fragment \"{}\" is defined on '{on}', not '{tn}'",
                                spread.fragment_name
                            ),
                        ));
                    }
                }
                flatten_into(&fragment.selection_set, fragments, vars, type_name, out)?;
            }
            Selection::InlineFragment(inline) => {
                if !directives_include(&inline.directives, vars)? {
                    continue;
                }
                if let (Some(graphql_parser::query::TypeCondition::On(on)), Some(tn)) =
                    (&inline.type_condition, type_name)
                {
                    if on != tn {
                        continue;
                    }
                }
                flatten_into(&inline.selection_set, fragments, vars, type_name, out)?;
            }
        }
    }
    Ok(())
}

fn directives_include(
    directives: &[graphql_parser::query::Directive<'static, String>],
    vars: &JsonMap<String, Json>,
) -> Result<bool, PlanError> {
    for d in directives {
        let arg = d
            .arguments
            .iter()
            .find(|(name, _)| name == "if")
            .map(|(_, v)| value_to_json(v, vars, "$"))
            .transpose()?
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        match d.name.as_str() {
            "include" if !arg => return Ok(false),
            "skip" if arg => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}
