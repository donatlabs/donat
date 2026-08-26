//! `invoke` — a cron or table event trigger that runs an existing action or
//! command inside the engine instead of posting an envelope to a URL.
//!
//! A webhook trigger hands the work to a receiver somebody has to write and
//! run. An `invoke` trigger has no receiver: it names an action or a command
//! already declared, says which classic role and which session variables the
//! call runs as — bound from the triggering row — and which arguments come
//! from which columns. The engine then takes the same path a GraphQL call
//! would, so there is no second permission world and no minted token.
//!
//! This module holds the declaration and the checks that need only the
//! metadata (a target that exists, a role the target admits, an argument
//! bound). Checks that need the catalog — the table and its columns — live
//! with `donat validate` in the server.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::types::{ActionEntry, Command, CronTrigger, EventTrigger, Metadata, QualifiedTable};

/// What a trigger runs in place of a webhook. Exactly one of `action` /
/// `command` names the target.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeTarget {
    /// An action from `actions.yaml`: its handler is called with `arguments`
    /// as the input, and `then` may hand the answer to a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// A command from `commands.yaml`, run directly with `arguments`. This is
    /// how a schedule starts a process: the command carries the
    /// `start_process` effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub session: InvokeSession,
    /// The rows a cron tick works through, one invocation each. Required on a
    /// cron trigger, refused on an event trigger (its row is the event's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreach: Option<Foreach>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, Bind>,
    /// A command to run over the action's answer. Only with `action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenCommand>,
}

/// The classic session the invocation runs as.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeSession {
    /// A classic role, listed on the target's permissions.
    pub role: String,
    /// Session variables (`x-donat-*` / `x-hasura-*`), each bound from the
    /// row. Keys are lowercased when the session is built.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, Bind>,
}

/// The rows of one table that are a cron tick's work items.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Foreach {
    #[serde(default = "default_source")]
    pub source: String,
    pub table: QualifiedTable,
    /// A closed predicate over the table's columns: `_is_null`, `_eq` against
    /// a literal, and `_and` of those. No session variable — there is no
    /// session yet — and no relationship.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "where")]
    pub where_: Option<serde_json::Value>,
    /// Array columns spread into one work item per element; the alias is a
    /// virtual column for binds and for `key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unnest: Vec<Unnest>,
    /// The columns (and unnest aliases) that identify one work item. Defaults
    /// to the table's primary key plus every unnest alias.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<String>,
}

fn default_source() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unnest {
    pub column: String,
    #[serde(rename = "as")]
    pub as_: String,
}

/// A command run once per item of the action's answer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThenCommand {
    /// `$` for the whole answer, or a dotted path into it. A list is one
    /// command per element; an object is one command; a missing path is no
    /// command at all.
    #[serde(default = "default_foreach")]
    pub foreach: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, Bind>,
}

fn default_foreach() -> String {
    "$".to_string()
}

/// Where one bound value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Bind {
    /// A column of the triggering row, or an unnest alias.
    Column { column: String },
    /// A value written in the declaration.
    Literal { literal: serde_json::Value },
    /// A dotted path into the current `then` item.
    Item { item: String },
    /// A session variable already bound on the invocation.
    Var { var: String },
}

/// The operators `foreach.where` may use. Closed on purpose: a background
/// tick reads rows no permission bounds, so what it can ask is kept small
/// enough to audit by eye.
const WHERE_OPERATORS: [&str; 2] = ["_is_null", "_eq"];

/// One trigger's `invoke`, whichever kind of trigger it sits on.
struct Site<'a> {
    what: String,
    invoke: &'a InvokeTarget,
    /// Cron: the tick has no row until `foreach` names one.
    cron: bool,
    /// Event: the trigger's own table.
    table: Option<&'a QualifiedTable>,
}

/// Every `invoke` in the metadata, and every trigger that has a target
/// problem (both, neither).
pub fn validate_invoke_targets(metadata: &Metadata) -> Vec<String> {
    let mut errors = Vec::new();
    let mut sites = Vec::new();
    for trigger in &metadata.cron_triggers {
        let what = format!("cron trigger '{}'", trigger.name);
        match cron_target(trigger) {
            Err(message) => errors.push(format!("{what}: {message}")),
            Ok(Some(invoke)) => sites.push(Site {
                what,
                invoke,
                cron: true,
                table: None,
            }),
            Ok(None) => {}
        }
    }
    for source in &metadata.sources {
        for table in &source.tables {
            for trigger in &table.event_triggers {
                let what = format!("event trigger '{}'", trigger.name);
                match event_target(trigger) {
                    Err(message) => errors.push(format!("{what}: {message}")),
                    Ok(Some(invoke)) => sites.push(Site {
                        what,
                        invoke,
                        cron: false,
                        table: Some(&table.table),
                    }),
                    Ok(None) => {}
                }
            }
        }
    }
    for site in sites {
        validate_site(metadata, &site, &mut errors);
    }
    errors
}

/// A cron trigger's target: the webhook, the invoke, or the reason it has
/// neither one exactly.
pub fn cron_target(trigger: &CronTrigger) -> Result<Option<&InvokeTarget>, String> {
    match (&trigger.webhook, &trigger.invoke) {
        (Some(_), Some(_)) => {
            Err("declares both `webhook` and `invoke`; a trigger has one target".into())
        }
        (None, None) => Err("declares neither `webhook` nor `invoke`".into()),
        (_, invoke) => Ok(invoke.as_ref()),
    }
}

pub fn event_target(trigger: &EventTrigger) -> Result<Option<&InvokeTarget>, String> {
    let webhook = trigger.webhook.is_some() || trigger.webhook_from_env.is_some();
    match (webhook, &trigger.invoke) {
        (true, Some(_)) => {
            Err("declares both a webhook and `invoke`; a trigger has one target".into())
        }
        (false, None) => Err("declares neither a webhook nor `invoke`".into()),
        (_, invoke) => Ok(invoke.as_ref()),
    }
}

fn validate_site(metadata: &Metadata, site: &Site<'_>, errors: &mut Vec<String>) {
    let what = &site.what;
    let invoke = site.invoke;
    let mut fail = |message: String| errors.push(format!("{what}: {message}"));

    // The row the binds read from.
    if site.cron && invoke.foreach.is_none() {
        fail("`invoke` on a cron trigger needs `foreach`: the tick has no row of its own".into());
    }
    if !site.cron && invoke.foreach.is_some() {
        fail("`invoke.foreach` is for cron triggers; an event trigger's row is the event's".into());
    }
    let mut aliases = BTreeSet::new();
    if let Some(foreach) = &invoke.foreach {
        if !metadata.sources.iter().any(|s| s.name == foreach.source) {
            fail(format!(
                "`foreach.source` '{}' is not a source",
                foreach.source
            ));
        }
        for unnest in &foreach.unnest {
            if !aliases.insert(unnest.as_.as_str()) {
                fail(format!(
                    "`foreach.unnest` alias '{}' is declared twice",
                    unnest.as_
                ));
            }
        }
        if let Some(where_) = &foreach.where_
            && let Err(message) = validate_where(where_)
        {
            fail(format!("`foreach.where`: {message}"));
        }
        // A declared key identifies one work item: every unnest alias is
        // part of it (two elements of one row are two items), and at least
        // one real column is, or the row could never be found again.
        if !foreach.key.is_empty() {
            for alias in &aliases {
                if !foreach.key.iter().any(|k| k == alias) {
                    fail(format!(
                        "`foreach.key` must include unnest alias '{alias}', or two elements \
                         of one row are the same work item"
                    ));
                }
            }
            if foreach.key.iter().all(|k| aliases.contains(k.as_str())) {
                fail(
                    "`foreach.key` names only unnest aliases; include a column of the table".into(),
                );
            }
        }
    }

    // The session.
    for name in invoke.session.vars.keys() {
        let lower = name.to_ascii_lowercase();
        if !(lower.starts_with("x-donat-") || lower.starts_with("x-hasura-")) {
            fail(format!(
                "session var '{name}' is not a session variable (x-donat-* / x-hasura-*)"
            ));
        }
        if lower == "x-donat-role" || lower == "x-hasura-role" {
            fail(format!(
                "session var '{name}' is the role; declare it as `session.role`"
            ));
        }
    }
    for (name, bind) in &invoke.session.vars {
        if let Bind::Item { .. } = bind {
            fail(format!(
                "session var '{name}' binds `item`, which exists only under `then`"
            ));
        }
    }
    let role = &invoke.session.role;

    // The target.
    let (action, command) = match (&invoke.action, &invoke.command) {
        (Some(_), Some(_)) => {
            fail("names both `action` and `command`; an invoke has one target".into());
            (None, None)
        }
        (None, None) => {
            fail("names neither `action` nor `command`".into());
            (None, None)
        }
        (Some(name), None) => {
            let action = metadata.actions.iter().find(|a| &a.name == name);
            if action.is_none() {
                fail(format!("action '{name}' does not exist"));
            }
            (action, None)
        }
        (None, Some(name)) => {
            let command = find_command(metadata, name, invoke.foreach.as_ref(), site.table);
            if command.is_none() {
                fail(format!("command '{name}' does not exist"));
            }
            if invoke.then.is_some() {
                fail("`then` follows an action's answer; a command target has none".into());
            }
            (None, command)
        }
    };
    if let Some(action) = action {
        if !crate::types::action_visible_to_role(action, role) {
            fail(format!(
                "role '{role}' is not in the permissions of action '{}'",
                action.name
            ));
        }
        check_action_arguments(action, &invoke.arguments, &mut fail);
    }
    if let Some(command) = command {
        check_command_role(command, role, &mut fail);
        check_command_arguments(command, &invoke.arguments, "arguments", &mut fail);
        check_tenant_var(metadata, command, &invoke.session, &mut fail);
    }
    for (name, bind) in &invoke.arguments {
        if let Bind::Item { .. } = bind {
            fail(format!(
                "argument '{name}' binds `item`, which exists only under `then`"
            ));
        }
    }

    // The follow-up.
    if let Some(then) = &invoke.then {
        let Some(command) =
            find_command(metadata, &then.command, invoke.foreach.as_ref(), site.table)
        else {
            fail(format!("`then.command` '{}' does not exist", then.command));
            return;
        };
        check_command_role(command, role, &mut fail);
        check_command_arguments(command, &then.arguments, "then.arguments", &mut fail);
        check_tenant_var(metadata, command, &invoke.session, &mut fail);
    }
}

/// A command by name. Commands are source-local; the one on the invoke's
/// source is meant, and the first one otherwise.
fn find_command<'a>(
    metadata: &'a Metadata,
    name: &str,
    foreach: Option<&Foreach>,
    _table: Option<&QualifiedTable>,
) -> Option<&'a Command> {
    let preferred = foreach.map(|f| f.source.as_str()).unwrap_or("default");
    metadata
        .commands
        .iter()
        .find(|c| c.name == name && c.source == preferred)
        .or_else(|| metadata.commands.iter().find(|c| c.name == name))
}

fn check_command_role(command: &Command, role: &str, fail: &mut impl FnMut(String)) {
    if !command.permissions.iter().any(|p| p.role == role) {
        fail(format!(
            "role '{role}' is not in the permissions of command '{}'",
            command.name
        ));
    }
}

fn check_action_arguments(
    action: &ActionEntry,
    bound: &BTreeMap<String, Bind>,
    fail: &mut impl FnMut(String),
) {
    for argument in &action.definition.arguments {
        if argument.type_.ends_with('!') && !bound.contains_key(&argument.name) {
            fail(format!(
                "argument '{}' of action '{}' is required and has no bind",
                argument.name, action.name
            ));
        }
    }
    for name in bound.keys() {
        if !action.definition.arguments.iter().any(|a| &a.name == name) {
            fail(format!(
                "'{name}' is not an argument of action '{}'",
                action.name
            ));
        }
    }
}

fn check_command_arguments(
    command: &Command,
    bound: &BTreeMap<String, Bind>,
    where_: &str,
    fail: &mut impl FnMut(String),
) {
    for argument in &command.arguments {
        if argument.type_.ends_with('!') && !bound.contains_key(&argument.name) {
            fail(format!(
                "{where_}: argument '{}' of command '{}' is required and has no bind",
                argument.name, command.name
            ));
        }
    }
    for name in bound.keys() {
        if !command.arguments.iter().any(|a| &a.name == name) {
            fail(format!(
                "{where_}: '{name}' is not an argument of command '{}'",
                command.name
            ));
        }
    }
}

/// A command on a tenanted source scopes every write by the tenant variable.
/// The invocation must bind it, or the command runs with no tenant at all —
/// unless the command is the one that establishes the tenant.
fn check_tenant_var(
    metadata: &Metadata,
    command: &Command,
    session: &InvokeSession,
    fail: &mut impl FnMut(String),
) {
    let Some(tenancy) = &metadata.tenancy else {
        return;
    };
    if tenancy.source != command.source || command.tenant.is_some() {
        return;
    }
    let variable = tenancy.variable_key();
    let bound = session
        .vars
        .keys()
        .any(|name| name.to_ascii_lowercase() == variable);
    if !bound {
        fail(format!(
            "command '{}' writes tenant-scoped rows but the session does not bind '{}'",
            command.name, tenancy.variable
        ));
    }
}

/// `{ col: { _is_null: bool } }`, `{ col: { _eq: literal } }`, `{ _and: [...] }`.
fn validate_where(where_: &serde_json::Value) -> Result<(), String> {
    let Some(map) = where_.as_object() else {
        return Err("expected an object".into());
    };
    for (key, value) in map {
        if key == "_and" {
            let Some(items) = value.as_array() else {
                return Err("`_and` takes a list".into());
            };
            for item in items {
                validate_where(item)?;
            }
            continue;
        }
        if key.starts_with('_') {
            return Err(format!(
                "operator '{key}' is outside the closed grammar (_is_null, _eq, _and)"
            ));
        }
        let Some(ops) = value.as_object() else {
            return Err(format!("column '{key}' takes an operator object"));
        };
        for (op, operand) in ops {
            if !WHERE_OPERATORS.contains(&op.as_str()) {
                return Err(format!(
                    "operator '{op}' on column '{key}' is outside the closed grammar \
                     (_is_null, _eq, _and)"
                ));
            }
            if op == "_is_null" && !operand.is_boolean() {
                return Err(format!("`_is_null` on column '{key}' takes true or false"));
            }
            if op == "_eq" && (operand.is_object() || operand.is_array()) {
                return Err(format!("`_eq` on column '{key}' takes a scalar literal"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(cron: serde_json::Value, extra: serde_json::Value) -> Metadata {
        let mut doc = json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgres://x" } },
                "tables": [{ "table": { "schema": "public", "name": "workspace" } }]
            }],
            "actions": [{
                "name": "linear_issues",
                "definition": {
                    "arguments": [
                        { "name": "token", "type": "String!" },
                        { "name": "teamId", "type": "String" }
                    ],
                    "output_type": "[Issue]",
                    "handler": "http://h"
                },
                "permissions": [{ "role": "user" }]
            }],
            "commands": [{
                "name": "ingest_issue",
                "source": "default",
                "permissions": [{ "role": "user" }],
                "arguments": [{ "name": "identifier", "type": "String!" }],
                "steps": []
            }],
            "cron_triggers": [cron]
        });
        if let Some(extra) = extra.as_object() {
            for (k, v) in extra {
                doc[k] = v.clone();
            }
        }
        serde_json::from_value(doc).expect("metadata")
    }

    fn invoke() -> serde_json::Value {
        json!({
            "action": "linear_issues",
            "session": { "role": "user", "vars": { "x-donat-user-id": { "column": "owner" } } },
            "foreach": { "table": { "schema": "public", "name": "workspace" } },
            "arguments": { "token": { "column": "linear_token" } }
        })
    }

    fn cron(fields: serde_json::Value) -> serde_json::Value {
        let mut c = json!({ "name": "pull", "schedule": "* * * * *" });
        for (k, v) in fields.as_object().unwrap() {
            c[k] = v.clone();
        }
        c
    }

    fn errors_of(cron_fields: serde_json::Value) -> Vec<String> {
        validate_invoke_targets(&metadata(cron(cron_fields), json!({})))
    }

    #[test]
    fn a_well_formed_invoke_passes() {
        assert_eq!(
            errors_of(json!({ "invoke": invoke() })),
            Vec::<String>::new()
        );
    }

    #[test]
    fn webhook_xor_invoke() {
        let both = errors_of(json!({ "webhook": "http://h", "invoke": invoke() }));
        assert!(both[0].contains("both `webhook` and `invoke`"), "{both:?}");
        let neither = errors_of(json!({}));
        assert!(neither[0].contains("neither"), "{neither:?}");
    }

    #[test]
    fn cron_invoke_needs_foreach() {
        let mut i = invoke();
        i.as_object_mut().unwrap().remove("foreach");
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors.iter().any(|e| e.contains("needs `foreach`")),
            "{errors:?}"
        );
    }

    #[test]
    fn role_must_be_on_the_action() {
        let mut i = invoke();
        i["session"]["role"] = json!("stranger");
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("role 'stranger' is not in the permissions of action")),
            "{errors:?}"
        );
    }

    #[test]
    fn required_argument_needs_a_bind_and_unknown_binds_are_refused() {
        let mut i = invoke();
        i["arguments"] = json!({ "teamId": { "literal": "T" }, "nope": { "literal": 1 } });
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("'token' of action 'linear_issues' is required")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("'nope' is not an argument")),
            "{errors:?}"
        );
    }

    #[test]
    fn unknown_targets_are_named() {
        let mut i = invoke();
        i["action"] = json!("ghost");
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("action 'ghost' does not exist")),
            "{errors:?}"
        );
        let mut i = invoke();
        i["then"] = json!({ "foreach": "$", "command": "ghost" });
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("`then.command` 'ghost' does not exist")),
            "{errors:?}"
        );
    }

    #[test]
    fn session_vars_are_session_variables() {
        let mut i = invoke();
        i["session"]["vars"] = json!({ "owner": { "column": "owner" } });
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("'owner' is not a session variable")),
            "{errors:?}"
        );
    }

    #[test]
    fn where_grammar_is_closed() {
        let mut i = invoke();
        i["foreach"]["where"] = json!({ "owner": { "_like": "a%" } });
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("'_like'") && e.contains("closed grammar")),
            "{errors:?}"
        );
        let mut i = invoke();
        i["foreach"]["where"] = json!({ "_and": [{ "owner": { "_eq": "a" } }, { "linear_token": { "_is_null": false } }] });
        assert_eq!(errors_of(json!({ "invoke": i })), Vec::<String>::new());
    }

    #[test]
    fn a_declared_key_carries_every_alias_and_a_real_column() {
        let mut i = invoke();
        i["foreach"]["unnest"] = json!([{ "column": "teams", "as": "team" }]);
        i["foreach"]["key"] = json!(["id"]);
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("must include unnest alias 'team'")),
            "{errors:?}"
        );
        let mut i = invoke();
        i["foreach"]["unnest"] = json!([{ "column": "teams", "as": "team" }]);
        i["foreach"]["key"] = json!(["team"]);
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("names only unnest aliases")),
            "{errors:?}"
        );
        let mut i = invoke();
        i["foreach"]["unnest"] = json!([{ "column": "teams", "as": "team" }]);
        i["foreach"]["key"] = json!(["id", "team"]);
        assert_eq!(errors_of(json!({ "invoke": i })), Vec::<String>::new());
    }

    #[test]
    fn a_command_target_takes_no_then() {
        let i = json!({
            "command": "ingest_issue",
            "session": { "role": "user" },
            "foreach": { "table": { "schema": "public", "name": "workspace" } },
            "arguments": { "identifier": { "literal": "X" } },
            "then": { "command": "ingest_issue" }
        });
        let errors = errors_of(json!({ "invoke": i }));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("a command target has none")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_tenanted_command_needs_the_tenant_var() {
        let i = json!({
            "command": "ingest_issue",
            "session": { "role": "user", "vars": { "x-donat-user-id": { "column": "owner" } } },
            "foreach": { "table": { "schema": "public", "name": "workspace" } },
            "arguments": { "identifier": { "literal": "X" } }
        });
        let md = metadata(
            cron(json!({ "invoke": i })),
            json!({ "tenancy": {
                "source": "default",
                "variable": "X-Donat-Tenant-Id",
                "key": "tenant_id",
                "registry": {
                    "table": { "schema": "public", "name": "tenant" },
                    "key": "id",
                    "status": { "column": "status", "serving": ["active"] }
                }
            } }),
        );
        let errors = validate_invoke_targets(&md);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("does not bind 'X-Donat-Tenant-Id'")),
            "{errors:?}"
        );
    }
}
