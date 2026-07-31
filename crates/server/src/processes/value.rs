//! Deterministic evaluation of deploy-time-validated Process value bindings.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail};
use donat_metadata::{ProcessBoundedFlatten, ProcessValue};
use serde_json::{Map as JsonMap, Value as Json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) struct ProcessValueContext<'a> {
    pub source_name: &'a str,
    pub instance_id: Uuid,
    pub input: &'a Json,
    pub state: &'a Json,
    pub caller_session: Option<&'a Json>,
    /// Database-owned time pinned by the event that made this state due.
    pub workflow_time: &'a Json,
    pub item: Option<&'a Json>,
    /// Canonical scalar identity for one bounded fan-out item. It is part of
    /// every activity key evaluated inside that item.
    pub item_key: Option<&'a str>,
}

pub(crate) fn evaluate_process_values(
    values: &BTreeMap<String, ProcessValue>,
    context: &ProcessValueContext<'_>,
) -> anyhow::Result<BTreeMap<String, Json>> {
    values
        .iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                evaluate_process_value(value, context)
                    .map_err(|error| anyhow!("Process binding `{name}` failed: {error}"))?,
            ))
        })
        .collect()
}

pub(crate) fn evaluate_process_value(
    value: &ProcessValue,
    context: &ProcessValueContext<'_>,
) -> anyhow::Result<Json> {
    match value {
        ProcessValue::Input {
            input,
            require_non_null,
            ..
        } => require_value(
            object_field(context.input, input, "input")?,
            *require_non_null,
            "input",
            input,
        ),
        ProcessValue::State {
            state,
            field,
            project,
            require_non_null,
            ..
        } => {
            let state_value = object_field(context.state, state, "state journal")?;
            let value = object_field(&state_value, field, "state output")?;
            let value = if let Some(project) = project {
                project_list(&value, project)?
            } else {
                value
            };
            require_value(value, *require_non_null, "state output", field)
        }
        ProcessValue::Item { item, .. } => {
            let item_value = context
                .item
                .ok_or_else(|| anyhow!("item binding is outside bounded for_each"))?;
            object_field(item_value, item, "for_each item")
        }
        ProcessValue::Literal { literal } => Ok(literal.clone()),
        ProcessValue::ActivityKey { activity_key, as_ } => Ok(activity_key_value(
            context,
            activity_key,
            as_.as_deref(),
            true,
        )),
        ProcessValue::ActivityKeyForState {
            activity_key_for_state,
            as_,
        } => Ok(activity_key_value(
            context,
            activity_key_for_state,
            as_.as_deref(),
            false,
        )),
        ProcessValue::Run { .. } => Ok(Json::String(context.instance_id.to_string())),
        ProcessValue::WorkflowTime { .. } => Ok(context.workflow_time.clone()),
        ProcessValue::SessionVariable { session_variable } => {
            let session = context
                .caller_session
                .ok_or_else(|| anyhow!("caller session is absent"))?;
            object_field(
                session,
                &session_variable.to_ascii_lowercase(),
                "caller session",
            )
        }
        ProcessValue::BoundedConcat { bounded_concat } => {
            if bounded_concat.inputs.len() > bounded_concat.maximum_lists as usize {
                bail!("bounded_concat exceeded maximum_lists");
            }
            let mut output = Vec::new();
            for input in &bounded_concat.inputs {
                let Json::Array(values) = evaluate_process_value(input, context)? else {
                    bail!("bounded_concat input is not a list");
                };
                output.extend(values);
                if output.len() > bounded_concat.maximum_items as usize {
                    bail!("bounded_concat exceeded maximum_items");
                }
            }
            Ok(Json::Array(output))
        }
        ProcessValue::BoundedFlatten { bounded_flatten } => {
            evaluate_bounded_flatten(bounded_flatten, context)
        }
    }
}

fn require_value(
    value: Json,
    require_non_null: bool,
    kind: &str,
    name: &str,
) -> anyhow::Result<Json> {
    if require_non_null && value.is_null() {
        bail!("{kind} `{name}` is null");
    }
    Ok(value)
}

fn object_field(value: &Json, field: &str, kind: &str) -> anyhow::Result<Json> {
    value
        .as_object()
        .and_then(|object| object.get(field))
        .cloned()
        .ok_or_else(|| anyhow!("{kind} field `{field}` is absent"))
}

fn project_list(value: &Json, fields: &[String]) -> anyhow::Result<Json> {
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("state project source is not a list"))?;
    values
        .iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| anyhow!("state project item is not an object"))?;
            let projected = fields
                .iter()
                .map(|field| {
                    object
                        .get(field)
                        .cloned()
                        .map(|value| (field.clone(), value))
                        .ok_or_else(|| anyhow!("state project field `{field}` is absent"))
                })
                .collect::<anyhow::Result<JsonMap<_, _>>>()?;
            Ok(Json::Object(projected))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json::Array)
}

fn activity_key_value(
    context: &ProcessValueContext<'_>,
    state: &str,
    cast: Option<&str>,
    include_item: bool,
) -> Json {
    let mut digest = Sha256::new();
    digest.update(b"donat.process.activity-key.v1\0");
    digest.update(context.source_name.as_bytes());
    digest.update(b"\0");
    digest.update(context.instance_id.as_bytes());
    digest.update(b"\0");
    digest.update(state.as_bytes());
    if include_item && let Some(item_key) = context.item_key {
        digest.update(b"\0item\0");
        digest.update(item_key.as_bytes());
    }
    let digest = digest.finalize();
    if cast == Some("uuid") {
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Json::String(Uuid::from_bytes(bytes).to_string())
    } else {
        Json::String(lower_hex(&digest))
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn evaluate_bounded_flatten(
    flattened: &ProcessBoundedFlatten,
    context: &ProcessValueContext<'_>,
) -> anyhow::Result<Json> {
    let source = evaluate_process_value(&flattened.from, context)?;
    let lists = source
        .as_array()
        .ok_or_else(|| anyhow!("bounded_flatten source is not a list"))?;
    if lists.len() > flattened.maximum_lists as usize {
        bail!("bounded_flatten exceeded maximum_lists");
    }
    let mut output = Vec::new();
    for value in lists {
        let nested = if let Some(field) = &flattened.field {
            object_field(value, field, "bounded_flatten item")?
        } else {
            value.clone()
        };
        let values = nested
            .as_array()
            .ok_or_else(|| anyhow!("bounded_flatten nested value is not a list"))?;
        for value in values {
            let value = if let Some(project) = &flattened.project {
                let object = value
                    .as_object()
                    .ok_or_else(|| anyhow!("bounded_flatten projected item is not an object"))?;
                let projected = project
                    .iter()
                    .map(|(target, source)| {
                        object
                            .get(source)
                            .cloned()
                            .map(|value| (target.clone(), value))
                            .ok_or_else(|| {
                                anyhow!("bounded_flatten projection field `{source}` is absent")
                            })
                    })
                    .collect::<anyhow::Result<JsonMap<_, _>>>()?;
                Json::Object(projected)
            } else {
                value.clone()
            };
            output.push(value);
            if output.len() > flattened.maximum_items as usize {
                bail!("bounded_flatten exceeded maximum_items");
            }
        }
    }
    Ok(Json::Array(output))
}
