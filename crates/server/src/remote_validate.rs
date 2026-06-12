//! Validation of role-based remote-schema SDL documents against the
//! upstream schema (fetched via introspection at add_remote_schema time).
//! Error strings replicate Hasura's reports verbatim.

use std::collections::HashMap;

use graphql_parser::schema::{Definition as SDef, Type, TypeDefinition, Value as SValue};
use serde_json::Value as Json;

/// Upstream type information distilled from an introspection response.
#[derive(Debug, Clone, Default)]
pub struct Upstream {
    pub types: HashMap<String, UpstreamType>,
}

#[derive(Debug, Clone)]
pub struct UpstreamType {
    pub kind: String,
    /// name -> (rendered type, args: name -> (rendered type, non_null))
    pub fields: HashMap<String, (String, HashMap<String, (String, bool)>)>,
    pub enum_values: Vec<String>,
    pub input_fields: HashMap<String, String>,
    pub union_members: Vec<String>,
    pub interfaces: Vec<String>,
}

fn render_type_ref(t: &Json) -> String {
    match t.get("kind").and_then(Json::as_str) {
        Some("NON_NULL") => format!("{}!", render_type_ref(&t["ofType"])),
        Some("LIST") => format!("[{}]", render_type_ref(&t["ofType"])),
        _ => t
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// Parse `__schema.types` from an introspection response.
pub fn parse_upstream(introspection: &Json) -> Upstream {
    let mut upstream = Upstream::default();
    let Some(types) = introspection
        .pointer("/data/__schema/types")
        .and_then(Json::as_array)
    else {
        return upstream;
    };
    for t in types {
        let Some(name) = t.get("name").and_then(Json::as_str) else {
            continue;
        };
        let mut ut = UpstreamType {
            kind: t
                .get("kind")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            fields: HashMap::new(),
            enum_values: vec![],
            input_fields: HashMap::new(),
            union_members: vec![],
            interfaces: vec![],
        };
        if let Some(fields) = t.get("fields").and_then(Json::as_array) {
            for f in fields {
                let Some(fname) = f.get("name").and_then(Json::as_str) else {
                    continue;
                };
                let mut args = HashMap::new();
                if let Some(fargs) = f.get("args").and_then(Json::as_array) {
                    for a in fargs {
                        let Some(aname) = a.get("name").and_then(Json::as_str) else {
                            continue;
                        };
                        let rendered = render_type_ref(&a["type"]);
                        let non_null = rendered.ends_with('!')
                            && a.get("defaultValue").map(Json::is_null).unwrap_or(true);
                        args.insert(aname.to_string(), (rendered, non_null));
                    }
                }
                ut.fields
                    .insert(fname.to_string(), (render_type_ref(&f["type"]), args));
            }
        }
        if let Some(values) = t.get("enumValues").and_then(Json::as_array) {
            for v in values {
                if let Some(n) = v.get("name").and_then(Json::as_str) {
                    ut.enum_values.push(n.to_string());
                }
            }
        }
        if let Some(inputs) = t.get("inputFields").and_then(Json::as_array) {
            for i in inputs {
                if let Some(n) = i.get("name").and_then(Json::as_str) {
                    ut.input_fields
                        .insert(n.to_string(), render_type_ref(&i["type"]));
                }
            }
        }
        if let Some(members) = t.get("possibleTypes").and_then(Json::as_array) {
            for m in members {
                if let Some(n) = m.get("name").and_then(Json::as_str) {
                    ut.union_members.push(n.to_string());
                }
            }
        }
        if let Some(ifaces) = t.get("interfaces").and_then(Json::as_array) {
            for i in ifaces {
                if let Some(n) = i.get("name").and_then(Json::as_str) {
                    ut.interfaces.push(n.to_string());
                }
            }
        }
        upstream.types.insert(name.to_string(), ut);
    }
    upstream
}

fn render_sdl_type(t: &Type<'static, String>) -> String {
    match t {
        Type::NamedType(n) => n.clone(),
        Type::ListType(inner) => format!("[{}]", render_sdl_type(inner)),
        Type::NonNullType(inner) => format!("{}!", render_sdl_type(inner)),
    }
}

/// Validate a role-based SDL document; Err holds Hasura's exact report.
pub fn validate(sdl_text: &str, upstream: &Upstream) -> Result<(), String> {
    let doc = match graphql_parser::parse_schema::<String>(sdl_text) {
        Ok(doc) => doc.into_static(),
        Err(e) => return Err(format!("invalid schema document: {e}")),
    };

    let mut reasons: Vec<String> = vec![];
    let builtin = ["Int", "Float", "String", "Boolean", "ID"];

    for def in &doc.definitions {
        let SDef::TypeDefinition(td) = def else { continue };
        match td {
            TypeDefinition::Scalar(s) => {
                if !builtin.contains(&s.name.as_str()) && !upstream.types.contains_key(&s.name)
                {
                    reasons.push(format!(
                        "\"Scalar\": \"{}\" does not exist in the upstream remote schema",
                        s.name
                    ));
                }
            }
            TypeDefinition::Enum(e) => {
                let Some(up) = upstream.types.get(&e.name) else {
                    reasons.push(format!(
                        "\"Enum\": \"{}\" does not exist in the upstream remote schema",
                        e.name
                    ));
                    continue;
                };
                let mut seen = std::collections::HashSet::new();
                let mut duplicates = vec![];
                let mut unknown = vec![];
                for v in &e.values {
                    if !seen.insert(v.name.clone()) {
                        duplicates.push(format!("\"{}\"", v.name));
                    } else if !up.enum_values.contains(&v.name) {
                        unknown.push(format!("\"{}\"", v.name));
                    }
                }
                if !unknown.is_empty() {
                    reasons.push(format!(
                        "enum \"{}\" contains the following enum values that do not exist in the corresponding upstream remote enum: {}",
                        e.name,
                        unknown.join(", ")
                    ));
                }
                if !duplicates.is_empty() {
                    reasons.push(format!(
                        "duplicate enum values: {} found in the \"{}\" enum",
                        duplicates.join(", "),
                        e.name
                    ));
                }
            }
            TypeDefinition::Union(u) => {
                if let Some(up) = upstream.types.get(&u.name) {
                    let bad: Vec<String> = u
                        .types
                        .iter()
                        .filter(|m| !up.union_members.contains(m))
                        .map(|m| format!("\"{m}\""))
                        .collect();
                    if !bad.is_empty() {
                        reasons.push(format!(
                            "union \"{}\" contains members which do not exist in the members of the remote schema union :{}",
                            u.name,
                            bad.join(", ")
                        ));
                    }
                } else {
                    reasons.push(format!(
                        "\"Union\": \"{}\" does not exist in the upstream remote schema",
                        u.name
                    ));
                }
            }
            TypeDefinition::Interface(i) => {
                let Some(up) = upstream.types.get(&i.name) else {
                    reasons.push(format!(
                        "\"Interface\": \"{}\" does not exist in the upstream remote schema",
                        i.name
                    ));
                    continue;
                };
                for f in &i.fields {
                    if !up.fields.contains_key(&f.name) {
                        reasons.push(format!(
                            "field \"{}\" does not exist in the \"Interface\": \"{}\"",
                            f.name, i.name
                        ));
                    }
                }
            }
            TypeDefinition::InputObject(io) => {
                for f in &io.fields {
                    for d in &f.directives {
                        if d.name == "preset" {
                            if let Some(reason) = validate_preset(&f.value_type, d, upstream) {
                                reasons.push(reason);
                            }
                        }
                    }
                }
                let Some(up) = upstream.types.get(&io.name) else {
                    reasons.push(format!(
                        "\"Input object\": \"{}\" does not exist in the upstream remote schema",
                        io.name
                    ));
                    continue;
                };
                for f in &io.fields {
                    match up.input_fields.get(&f.name) {
                        None => reasons.push(format!(
                            "\"{}\" does not exist in the input object \"{}\"",
                            f.name, io.name
                        )),
                        Some(expected) => {
                            let got = render_sdl_type(&f.value_type);
                            if expected != &got {
                                reasons.push(format!(
                                    "expected type of \"{}\"(\"Input object argument\") to be {} but received {}",
                                    f.name, expected, got
                                ));
                            }
                        }
                    }
                }
            }
            TypeDefinition::Object(o) => {
                let Some(up) = upstream.types.get(&o.name) else {
                    reasons.push(format!(
                        "\"Object\": \"{}\" does not exist in the upstream remote schema",
                        o.name
                    ));
                    continue;
                };
                // Interfaces: must exist upstream AND be among the
                // object's upstream interfaces.
                let mut custom_ifaces = vec![];
                for iface in &o.implements_interfaces {
                    if !upstream.types.contains_key(iface) {
                        reasons.push(format!(
                            "\"Interface\": \"{iface}\" does not exist in the upstream remote schema"
                        ));
                        custom_ifaces.push(format!("\"{iface}\""));
                    } else if !up.interfaces.contains(iface) {
                        custom_ifaces.push(format!("\"{iface}\""));
                    }
                }
                if !custom_ifaces.is_empty() {
                    reasons.push(format!(
                        "custom interfaces are not supported. Object\"{}\" implements the following custom interfaces: {}",
                        o.name,
                        custom_ifaces.join(", ")
                    ));
                }
                for f in &o.fields {
                    let Some((up_type, up_args)) = up.fields.get(&f.name) else {
                        reasons.push(format!(
                            "field \"{}\" does not exist in the \"Object\": \"{}\"",
                            f.name, o.name
                        ));
                        continue;
                    };
                    let got = render_sdl_type(&f.field_type);
                    if up_type != &got {
                        reasons.push(format!(
                            "expected type of \"{}\"(\"Object field\") to be {} but received {}",
                            f.name, up_type, got
                        ));
                    }
                    // Non-nullable upstream args must be present.
                    let missing: Vec<String> = up_args
                        .iter()
                        .filter(|(name, (_, non_null))| {
                            *non_null && !f.arguments.iter().any(|a| &a.name == *name)
                        })
                        .map(|(name, _)| format!("\"{name}\""))
                        .collect();
                    if !missing.is_empty() {
                        reasons.push(format!(
                            "field: \"{}\" expects the following non nullable arguments to be present: {}",
                            f.name,
                            missing.join(", ")
                        ));
                    }
                    // Argument types + preset directive placement/shape.
                    for a in &f.arguments {
                        for d in &a.directives {
                            if d.name == "preset" {
                                if let Some(reason) =
                                    validate_preset(&a.value_type, d, upstream)
                                {
                                    reasons.push(reason);
                                }
                            }
                        }
                    }
                }
            }
        }
        // @preset anywhere else is rejected via directive scan below.
    }

    // Preset directives are only legal on arguments / input fields.
    for def in &doc.definitions {
        if let SDef::TypeDefinition(TypeDefinition::Object(o)) = def {
            for f in &o.fields {
                for d in &f.directives {
                    if d.name == "preset" {
                        reasons.push(
                            "Preset directives can be defined only on INPUT_FIELD_DEFINITION or ARGUMENT_DEFINITION"
                                .to_string(),
                        );
                    }
                }
            }
        }
    }

    // The same defect can be discovered by more than one rule (e.g. a
    // missing interface, reported both at its definition and at an object
    // implementing it); Hasura reports each reason once.
    let mut seen = std::collections::HashSet::new();
    reasons.retain(|r| seen.insert(r.clone()));

    match reasons.len() {
        0 => Ok(()),
        1 => Err(format!(
            "validation for the given role-based schema failed because {}",
            reasons[0]
        )),
        _ => {
            let mut out = String::from(
                "validation for the given role-based schema failed for the following reasons:\n",
            );
            for r in &reasons {
                out.push_str(&format!(" • {r}\n"));
            }
            Err(out)
        }
    }
}

/// Preset value must match the argument's (input object) shape.
fn validate_preset(
    arg_type: &Type<'static, String>,
    directive: &graphql_parser::schema::Directive<'static, String>,
    upstream: &Upstream,
) -> Option<String> {
    let value = directive
        .arguments
        .iter()
        .find(|(n, _)| n == "value")
        .map(|(_, v)| v)?;
    let type_name = match arg_type {
        Type::NamedType(n) => n.clone(),
        Type::ListType(_) => return None,
        Type::NonNullType(inner) => match inner.as_ref() {
            Type::NamedType(n) => n.clone(),
            _ => return None,
        },
    };
    let Some(up) = upstream.types.get(&type_name) else {
        return None;
    };
    if up.kind == "INPUT_OBJECT" {
        match value {
            SValue::Object(map) => {
                for key in map.keys() {
                    if !up.input_fields.contains_key(key) {
                        return Some(format!(
                            "\"{key}\" does not exist in the input object \"{type_name}\""
                        ));
                    }
                }
                None
            }
            other => Some(format!(
                "expected preset value \"{other}\" of type \"{type_name}\" to be an input object value"
            )),
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ty(kind: &str) -> UpstreamType {
        UpstreamType {
            kind: kind.to_string(),
            fields: HashMap::new(),
            enum_values: vec![],
            input_fields: HashMap::new(),
            union_members: vec![],
            interfaces: vec![],
        }
    }

    /// Upstream: Query { user(id: Int!): User, name: String },
    /// User { id: ID!, name: String }, enum Color { RED GREEN },
    /// input Filter { limit: Int }.
    fn upstream() -> Upstream {
        let mut up = Upstream::default();
        let mut query = ty("OBJECT");
        let mut user_args = HashMap::new();
        user_args.insert("id".to_string(), ("Int!".to_string(), true));
        query
            .fields
            .insert("user".to_string(), ("User".to_string(), user_args));
        query
            .fields
            .insert("name".to_string(), ("String".to_string(), HashMap::new()));
        up.types.insert("Query".to_string(), query);

        let mut user = ty("OBJECT");
        user.fields
            .insert("id".to_string(), ("ID!".to_string(), HashMap::new()));
        user.fields
            .insert("name".to_string(), ("String".to_string(), HashMap::new()));
        up.types.insert("User".to_string(), user);

        let mut color = ty("ENUM");
        color.enum_values = vec!["RED".to_string(), "GREEN".to_string()];
        up.types.insert("Color".to_string(), color);

        let mut filter = ty("INPUT_OBJECT");
        filter.input_fields.insert("limit".to_string(), "Int".to_string());
        up.types.insert("Filter".to_string(), filter);
        up
    }

    #[test]
    fn valid_subset_passes() {
        let up = upstream();
        assert_eq!(
            validate(
                "type Query { user(id: Int!): User } type User { id: ID! } scalar Int",
                &up
            ),
            Ok(())
        );
    }

    #[test]
    fn unknown_scalar_single_reason_format() {
        let e = validate("scalar Odd", &upstream()).unwrap_err();
        assert_eq!(
            e,
            "validation for the given role-based schema failed because \"Scalar\": \"Odd\" does not exist in the upstream remote schema"
        );
    }

    #[test]
    fn enum_value_mismatches() {
        let e = validate("enum Color { RED BLUE BLUE }", &upstream()).unwrap_err();
        assert!(
            e.starts_with("validation for the given role-based schema failed for the following reasons:\n"),
            "{e}"
        );
        assert!(
            e.contains(" • enum \"Color\" contains the following enum values that do not exist in the corresponding upstream remote enum: \"BLUE\"\n"),
            "{e}"
        );
        assert!(
            e.contains(" • duplicate enum values: \"BLUE\" found in the \"Color\" enum\n"),
            "{e}"
        );
    }

    #[test]
    fn object_field_type_mismatch_message() {
        let e = validate("type User { id: ID }", &upstream()).unwrap_err();
        assert_eq!(
            e,
            "validation for the given role-based schema failed because expected type of \"id\"(\"Object field\") to be ID! but received ID"
        );
    }

    #[test]
    fn object_unknown_field_message() {
        let e = validate("type User { nope: String }", &upstream()).unwrap_err();
        assert_eq!(
            e,
            "validation for the given role-based schema failed because field \"nope\" does not exist in the \"Object\": \"User\""
        );
    }

    #[test]
    fn missing_non_nullable_argument() {
        let e = validate("type Query { user: User } type User { id: ID! }", &upstream())
            .unwrap_err();
        assert_eq!(
            e,
            "validation for the given role-based schema failed because field: \"user\" expects the following non nullable arguments to be present: \"id\""
        );
    }

    #[test]
    fn dangling_interface_reported_once() {
        // The missing interface is discovered both at its definition and
        // at the object implementing it; the report carries it once.
        let sdl = "interface Node { id: ID! } type User implements Node { id: ID! }";
        let e = validate(sdl, &upstream()).unwrap_err();
        assert_eq!(
            e.matches("\"Interface\": \"Node\" does not exist in the upstream remote schema")
                .count(),
            1,
            "{e}"
        );
        assert!(
            e.contains("custom interfaces are not supported. Object\"User\" implements the following custom interfaces: \"Node\""),
            "{e}"
        );
    }

    #[test]
    fn preset_only_on_arguments_or_input_fields() {
        let e = validate("type Query { name: String @preset(value: 5) }", &upstream())
            .unwrap_err();
        assert_eq!(
            e,
            "validation for the given role-based schema failed because Preset directives can be defined only on INPUT_FIELD_DEFINITION or ARGUMENT_DEFINITION"
        );
    }

    #[test]
    fn input_object_field_mismatches() {
        let e = validate("input Filter { limit: String nope: Int }", &upstream()).unwrap_err();
        assert!(
            e.contains(" • expected type of \"limit\"(\"Input object argument\") to be Int but received String"),
            "{e}"
        );
        assert!(
            e.contains(" • \"nope\" does not exist in the input object \"Filter\""),
            "{e}"
        );
    }

    #[test]
    fn parse_upstream_renders_wrapped_types_and_required_args() {
        let intro = json!({ "data": { "__schema": { "types": [
            { "kind": "OBJECT", "name": "Query", "fields": [
                { "name": "users",
                  "args": [
                    { "name": "limit", "defaultValue": "10",
                      "type": { "kind": "NON_NULL", "ofType": { "kind": "SCALAR", "name": "Int" } } },
                    { "name": "id", "defaultValue": null,
                      "type": { "kind": "NON_NULL", "ofType": { "kind": "SCALAR", "name": "Int" } } }
                  ],
                  "type": { "kind": "NON_NULL", "ofType": {
                      "kind": "LIST", "ofType": { "kind": "OBJECT", "name": "User" } } } }
            ] },
            { "kind": "ENUM", "name": "Color", "enumValues": [ { "name": "RED" } ] }
        ] } } });
        let up = parse_upstream(&intro);
        let (rendered, args) = &up.types["Query"].fields["users"];
        assert_eq!(rendered, "[User]!");
        assert_eq!(args["id"], ("Int!".to_string(), true));
        // A default value makes a non-null argument optional.
        assert_eq!(args["limit"], ("Int!".to_string(), false));
        assert_eq!(up.types["Color"].enum_values, vec!["RED"]);
    }
}
