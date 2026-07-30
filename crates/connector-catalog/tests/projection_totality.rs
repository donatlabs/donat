use std::collections::{BTreeMap, BTreeSet};

use donat_connector_catalog::{
    CANONICAL_PROJECTION_SCHEMA_DECLARATIONS, canonical_projection_owner_manifest,
};
use syn::{
    Attribute, Fields, FnArg, GenericArgument, Item, LitStr, PathArguments, Type, TypePath,
    Visibility,
};

#[derive(Clone)]
struct Field {
    rust_name: Option<String>,
    wire_name: Option<String>,
    ty: Type,
}

#[derive(Clone)]
struct Variant {
    rust_name: String,
    wire_name: String,
    fields: Vec<Field>,
}

#[derive(Clone)]
enum Shape {
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
}

#[derive(Clone)]
struct Definition {
    public: bool,
    shape: Shape,
}

#[derive(Default)]
struct Schema {
    definitions: BTreeMap<String, Definition>,
    extension_fields: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Segment {
    Field(String),
    RustVariant(String),
    WireVariant(String),
}

#[derive(Clone)]
enum Cursor {
    Definition(String),
    Variant {
        definition: String,
        variant: Variant,
    },
    Value(Type),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Member {
    definition: String,
    member: String,
}

impl Member {
    fn field(definition: &str, field: &str) -> Self {
        Self {
            definition: definition.to_owned(),
            member: field.to_owned(),
        }
    }

    fn variant(definition: &str, variant: &str) -> Self {
        Self {
            definition: definition.to_owned(),
            member: format!("::{variant}"),
        }
    }

    fn variant_field(definition: &str, variant: &str, field: &str) -> Self {
        Self {
            definition: definition.to_owned(),
            member: format!("::{variant}.{field}"),
        }
    }
}

fn serde_rename(attributes: &[Attribute]) -> Option<String> {
    let mut rename = None;
    for attribute in attributes {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                rename = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            Ok(())
        });
    }
    rename
}

fn snake_case(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, character) in characters.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|value| characters.get(value));
        let next = characters.get(index + 1);
        if character.is_ascii_uppercase()
            && index != 0
            && (previous.is_some_and(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
                || next.is_some_and(|value| value.is_ascii_lowercase()))
        {
            output.push('_');
        }
        output.push(character.to_ascii_lowercase());
    }
    output
}

fn parse_field(rust_name: Option<String>, attributes: &[Attribute], ty: Type) -> Field {
    Field {
        wire_name: serde_rename(attributes).or_else(|| rust_name.clone()),
        rust_name,
        ty,
    }
}

fn collect_schema(source: &str) -> Schema {
    let file = syn::parse_file(source).unwrap();
    let mut schema = Schema::default();
    for item in &file.items {
        match item {
            Item::Struct(value) => {
                let fields = match &value.fields {
                    Fields::Named(fields) => fields
                        .named
                        .iter()
                        .map(|field| {
                            parse_field(
                                field.ident.as_ref().map(ToString::to_string),
                                &field.attrs,
                                field.ty.clone(),
                            )
                        })
                        .collect(),
                    Fields::Unnamed(fields) => fields
                        .unnamed
                        .iter()
                        .map(|field| parse_field(None, &field.attrs, field.ty.clone()))
                        .collect(),
                    Fields::Unit => Vec::new(),
                };
                schema.definitions.insert(
                    value.ident.to_string(),
                    Definition {
                        public: matches!(value.vis, Visibility::Public(_)),
                        shape: Shape::Struct(fields),
                    },
                );
            }
            Item::Enum(value) => {
                let variants = value
                    .variants
                    .iter()
                    .map(|variant| {
                        let rust_name = variant.ident.to_string();
                        Variant {
                            wire_name: serde_rename(&variant.attrs)
                                .unwrap_or_else(|| snake_case(&rust_name)),
                            rust_name,
                            fields: match &variant.fields {
                                Fields::Named(fields) => fields
                                    .named
                                    .iter()
                                    .map(|field| {
                                        parse_field(
                                            field.ident.as_ref().map(ToString::to_string),
                                            &field.attrs,
                                            field.ty.clone(),
                                        )
                                    })
                                    .collect(),
                                Fields::Unnamed(fields) => fields
                                    .unnamed
                                    .iter()
                                    .map(|field| parse_field(None, &field.attrs, field.ty.clone()))
                                    .collect(),
                                Fields::Unit => Vec::new(),
                            },
                        }
                    })
                    .collect();
                schema.definitions.insert(
                    value.ident.to_string(),
                    Definition {
                        public: matches!(value.vis, Visibility::Public(_)),
                        shape: Shape::Enum(variants),
                    },
                );
            }
            _ => {}
        }
    }
    schema
}

fn merge_schema(target: &mut Schema, source: Schema) {
    target.definitions.extend(source.definitions);
    for (owner, fields) in source.extension_fields {
        target
            .extension_fields
            .entry(owner)
            .or_default()
            .extend(fields);
    }
}

fn type_name(value: &Type) -> Option<String> {
    match value {
        Type::Path(TypePath { path, .. }) => {
            let segment = path.segments.last()?;
            match &segment.arguments {
                PathArguments::None => Some(segment.ident.to_string()),
                PathArguments::AngleBracketed(arguments) => arguments
                    .args
                    .iter()
                    .filter_map(|argument| match argument {
                        GenericArgument::Type(value) => Some(value),
                        _ => None,
                    })
                    .next_back()
                    .and_then(type_name),
                PathArguments::Parenthesized(_) => None,
            }
        }
        Type::Array(value) => type_name(&value.elem),
        Type::Group(value) => type_name(&value.elem),
        Type::Paren(value) => type_name(&value.elem),
        Type::Reference(value) => type_name(&value.elem),
        Type::Slice(value) => type_name(&value.elem),
        Type::Tuple(_) => None,
        _ => None,
    }
}

fn collect_builder_extensions(schema: &mut Schema, source: &str) {
    let file = syn::parse_file(source).unwrap();
    for item in file.items {
        let Item::Fn(function) = item else {
            continue;
        };
        if !matches!(function.vis, Visibility::Public(_)) {
            continue;
        }
        let arguments = function
            .sig
            .inputs
            .iter()
            .filter_map(|argument| match argument {
                FnArg::Typed(argument) => {
                    let syn::Pat::Ident(name) = argument.pat.as_ref() else {
                        return None;
                    };
                    Some((name.ident.to_string(), argument.ty.as_ref()))
                }
                FnArg::Receiver(_) => None,
            })
            .collect::<Vec<_>>();
        for (_, owner_type) in &arguments {
            let Some(owner) = type_name(owner_type) else {
                continue;
            };
            for (name, sibling_type) in &arguments {
                if type_name(sibling_type).as_deref() != Some(owner.as_str()) {
                    schema
                        .extension_fields
                        .entry(owner.clone())
                        .or_default()
                        .insert(name.clone());
                }
            }
        }
    }
}

fn normalized_schema() -> Schema {
    let mut schema = Schema::default();
    for source in [
        include_str!("../src/source.rs"),
        include_str!("../src/model.rs"),
        include_str!("../../value-contract/src/lib.rs"),
        include_str!("../../connector-abi/src/lib.rs"),
        include_str!("../../connector-abi/src/envelope.rs"),
    ] {
        merge_schema(&mut schema, collect_schema(source));
    }
    collect_builder_extensions(&mut schema, include_str!("../src/canonical.rs"));
    schema
}

fn material_schema() -> Schema {
    let mut schema = normalized_schema();
    merge_schema(
        &mut schema,
        collect_schema(CANONICAL_PROJECTION_SCHEMA_DECLARATIONS),
    );
    schema
}

fn owner_root(value: &str) -> &str {
    value
        .split(['.', ':', '{'])
        .next()
        .expect("owner expression always has a root")
}

fn immediate_member(value: &str) -> Option<String> {
    let root = owner_root(value);
    let suffix = value.strip_prefix(root)?;
    if let Some(suffix) = suffix.strip_prefix("::") {
        return Some(
            suffix
                .split(['.', ':'])
                .next()
                .unwrap()
                .trim_end_matches("[]")
                .to_owned(),
        );
    }
    suffix.strip_prefix('.').map(|suffix| {
        suffix
            .split(['.', ':'])
            .next()
            .unwrap()
            .trim_end_matches("[]")
            .to_owned()
    })
}

fn structural_roots(schema: &Schema, expressions: &[&str]) -> BTreeMap<String, String> {
    let mut requirements = BTreeMap::<String, BTreeSet<String>>::new();
    for expression in expressions {
        let root = owner_root(expression);
        if !schema.definitions.contains_key(root)
            && let Some(member) = immediate_member(expression)
        {
            requirements
                .entry(root.to_owned())
                .or_default()
                .insert(member);
        }
    }
    requirements
        .into_iter()
        .map(|(logical, expected)| {
            let candidates = schema
                .definitions
                .iter()
                .filter_map(|(name, definition)| {
                    let actual = match &definition.shape {
                        Shape::Struct(fields) => fields
                            .iter()
                            .filter_map(|field| field.rust_name.clone())
                            .collect::<BTreeSet<_>>(),
                        Shape::Enum(variants) => variants
                            .iter()
                            .map(|variant| variant.rust_name.clone())
                            .collect::<BTreeSet<_>>(),
                    };
                    (actual == expected).then_some(name.clone())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                candidates.len(),
                1,
                "logical owner {logical} does not resolve by its complete structure: {candidates:?}"
            );
            (logical, candidates[0].clone())
        })
        .collect()
}

fn tokenize(value: &str) -> (String, Vec<Segment>) {
    let bytes = value.as_bytes();
    let root_end = value.find(['.', ':', '{']).unwrap_or(value.len());
    let root = value[..root_end].to_owned();
    let mut segments = Vec::new();
    let mut index = root_end;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"[]") {
            index += 2;
            continue;
        }
        if bytes[index..].starts_with(b"::") {
            index += 2;
            let end = value[index..]
                .find(['.', ':', '{', '['])
                .map_or(value.len(), |offset| index + offset);
            segments.push(Segment::RustVariant(value[index..end].to_owned()));
            index = end;
            continue;
        }
        if bytes[index] == b'{' {
            let end = value[index..].find('}').unwrap() + index;
            let selector = &value[index + 1..end];
            segments.push(Segment::WireVariant(
                selector.strip_prefix("kind=").unwrap().to_owned(),
            ));
            index = end + 1;
            continue;
        }
        if bytes[index] == b'.' {
            index += 1;
            let end = value[index..]
                .find(['.', ':', '{', '['])
                .map_or(value.len(), |offset| index + offset);
            segments.push(Segment::Field(value[index..end].to_owned()));
            index = end;
            continue;
        }
        panic!("unparsed schema expression suffix: {}", &value[index..]);
    }
    (root, segments)
}

fn transparent_definition(schema: &Schema, mut cursor: Cursor) -> Cursor {
    loop {
        let Cursor::Definition(name) = &cursor else {
            return cursor;
        };
        let Some(Definition {
            shape: Shape::Struct(fields),
            ..
        }) = schema.definitions.get(name)
        else {
            return cursor;
        };
        if fields.len() != 1 || fields[0].rust_name.is_some() {
            return cursor;
        }
        cursor = Cursor::Value(fields[0].ty.clone());
    }
}

fn value_definition(schema: &Schema, value: &Type) -> Option<String> {
    let name = type_name(value)?;
    schema.definitions.contains_key(&name).then_some(name)
}

fn select_variant(
    schema: &Schema,
    definition: &str,
    selector: &Segment,
) -> Option<(String, Variant)> {
    fn visit(
        schema: &Schema,
        definition: &str,
        selector: &Segment,
        visited: &mut BTreeSet<String>,
    ) -> Vec<(String, Variant)> {
        if !visited.insert(definition.to_owned()) {
            return Vec::new();
        }
        let Some(definition_shape) = schema.definitions.get(definition) else {
            return Vec::new();
        };
        let mut matches = Vec::new();
        match &definition_shape.shape {
            Shape::Enum(variants) => {
                matches.extend(
                    variants
                        .iter()
                        .filter(|variant| match selector {
                            Segment::RustVariant(name) => variant.rust_name == *name,
                            Segment::WireVariant(name) => variant.wire_name == *name,
                            Segment::Field(_) => false,
                        })
                        .cloned()
                        .map(|variant| (definition.to_owned(), variant)),
                );
                for variant in variants {
                    if variant.fields.len() == 1
                        && variant.fields[0].rust_name.is_none()
                        && let Some(payload) = value_definition(schema, &variant.fields[0].ty)
                    {
                        matches.extend(visit(schema, &payload, selector, visited));
                    }
                }
            }
            Shape::Struct(fields) => {
                for field in fields {
                    if let Some(payload) = value_definition(schema, &field.ty) {
                        matches.extend(visit(schema, &payload, selector, visited));
                    }
                }
            }
        }
        matches
    }

    let matches = visit(schema, definition, selector, &mut BTreeSet::new());
    (matches.len() == 1).then(|| matches[0].clone())
}

fn resolve_expression(
    schema: &Schema,
    structural: &BTreeMap<String, String>,
    expression: &str,
) -> Result<(Member, BTreeSet<Member>), String> {
    let (logical_root, segments) = tokenize(expression);
    let root = structural
        .get(&logical_root)
        .cloned()
        .unwrap_or(logical_root);
    let mut cursor = Cursor::Definition(root);
    let mut terminal = None;
    let mut traversed = BTreeSet::new();
    let mut index = 0;
    while index < segments.len() {
        cursor = transparent_definition(schema, cursor);
        if let Cursor::Value(value) = &cursor {
            let Some(definition) = value_definition(schema, value) else {
                return Err(format!(
                    "{expression}: primitive value has trailing segment {:?}",
                    segments[index]
                ));
            };
            cursor = Cursor::Definition(definition);
            continue;
        }
        match (&cursor, &segments[index]) {
            (Cursor::Definition(definition), selector @ Segment::RustVariant(_))
            | (Cursor::Definition(definition), selector @ Segment::WireVariant(_)) => {
                let (owner, variant) = select_variant(schema, definition, selector)
                    .ok_or_else(|| format!("{expression}: unknown variant {selector:?}"))?;
                let member = Member::variant(&owner, &variant.rust_name);
                traversed.insert(member.clone());
                terminal = Some(member);
                cursor = Cursor::Variant {
                    definition: owner,
                    variant,
                };
                index += 1;
            }
            (Cursor::Definition(definition), Segment::Field(name)) => {
                let Some(owner) = schema.definitions.get(definition) else {
                    return Err(format!("{expression}: unknown definition {definition}"));
                };
                let Shape::Struct(fields) = &owner.shape else {
                    return Err(format!("{expression}: enum field requires a branch"));
                };
                if let Some(field) = fields
                    .iter()
                    .find(|field| field.wire_name.as_deref() == Some(name))
                {
                    let field_name = field.rust_name.as_deref().unwrap_or(name);
                    let member = Member::field(definition, field_name);
                    traversed.insert(member.clone());
                    terminal = Some(member);
                    cursor = Cursor::Value(field.ty.clone());
                    index += 1;
                } else if schema
                    .extension_fields
                    .get(definition)
                    .is_some_and(|fields| fields.contains(name))
                {
                    let member = Member::field(definition, name);
                    traversed.insert(member.clone());
                    terminal = Some(member);
                    index += 1;
                } else {
                    return Err(format!("{expression}: unknown field {definition}.{name}"));
                }
            }
            (
                Cursor::Variant {
                    definition,
                    variant,
                },
                Segment::Field(name),
            ) if name == "kind" => {
                let member = Member::variant(definition, &variant.rust_name);
                traversed.insert(member.clone());
                terminal = Some(member);
                index += 1;
            }
            (
                Cursor::Variant {
                    definition,
                    variant,
                },
                Segment::Field(name),
            ) if name == "value" => {
                if index + 1 == segments.len() {
                    let member = Member::variant_field(definition, &variant.rust_name, "value");
                    traversed.insert(member.clone());
                    terminal = Some(member);
                    index += 1;
                } else if variant.fields.len() == 1 && variant.fields[0].rust_name.is_none() {
                    traversed.insert(Member::variant_field(
                        definition,
                        &variant.rust_name,
                        "value",
                    ));
                    cursor = Cursor::Value(variant.fields[0].ty.clone());
                    index += 1;
                } else {
                    index += 1;
                }
            }
            (
                Cursor::Variant {
                    definition,
                    variant,
                },
                Segment::Field(name),
            ) => {
                if let Some(field) = variant
                    .fields
                    .iter()
                    .find(|field| field.wire_name.as_deref() == Some(name))
                {
                    let member = Member::variant_field(
                        definition,
                        &variant.rust_name,
                        field.rust_name.as_deref().unwrap_or(name),
                    );
                    traversed.insert(member.clone());
                    terminal = Some(member);
                    cursor = Cursor::Value(field.ty.clone());
                    index += 1;
                } else if variant.fields.len() == 1 && variant.fields[0].rust_name.is_none() {
                    cursor = Cursor::Value(variant.fields[0].ty.clone());
                } else {
                    return Err(format!(
                        "{expression}: unknown branch field {definition}::{}.{name}",
                        variant.rust_name
                    ));
                }
            }
            (_, segment) => {
                return Err(format!("{expression}: cannot resolve segment {segment:?}"));
            }
        }
    }
    terminal
        .map(|terminal| (terminal, traversed))
        .ok_or_else(|| format!("{expression}: expression has no material member"))
}

fn declared_members(
    schema: &Schema,
    definitions: &BTreeSet<String>,
    tagged_values: bool,
) -> BTreeSet<Member> {
    fn serializes_as_variant_value(schema: &Schema, ty: &Type) -> bool {
        match ty {
            Type::Tuple(tuple) => !tuple.elems.is_empty(),
            Type::Path(TypePath { path, .. }) => {
                let Some(segment) = path.segments.last() else {
                    return false;
                };
                if matches!(
                    segment.ident.to_string().as_str(),
                    "Vec" | "BTreeMap" | "BTreeSet" | "Option"
                ) {
                    return true;
                }
                let name = segment.ident.to_string();
                match schema
                    .definitions
                    .get(&name)
                    .map(|definition| &definition.shape)
                {
                    Some(Shape::Struct(fields))
                        if fields.len() == 1 && fields[0].rust_name.is_none() =>
                    {
                        serializes_as_variant_value(schema, &fields[0].ty)
                    }
                    Some(_) => false,
                    None => true,
                }
            }
            Type::Array(_) | Type::Slice(_) => true,
            Type::Group(value) => serializes_as_variant_value(schema, &value.elem),
            Type::Paren(value) => serializes_as_variant_value(schema, &value.elem),
            Type::Reference(value) => serializes_as_variant_value(schema, &value.elem),
            _ => true,
        }
    }

    let mut output = BTreeSet::new();
    for name in definitions {
        let Some(definition) = schema.definitions.get(name) else {
            continue;
        };
        match &definition.shape {
            Shape::Struct(fields) => {
                output.extend(fields.iter().filter_map(|field| {
                    field
                        .rust_name
                        .as_deref()
                        .map(|field| Member::field(name, field))
                }));
            }
            Shape::Enum(variants) => {
                for variant in variants {
                    let flattened_enum = variant.fields.len() == 1
                        && variant.fields[0].rust_name.is_none()
                        && matches!(
                            &variant.fields[0].ty,
                            Type::Path(TypePath { path, .. })
                                if path.segments.last().is_some_and(|segment| {
                                    matches!(segment.arguments, PathArguments::None)
                                        && schema.definitions.get(&segment.ident.to_string()).is_some_and(
                                            |definition| matches!(definition.shape, Shape::Enum(_))
                                        )
                                })
                        );
                    if flattened_enum {
                        continue;
                    }
                    output.insert(Member::variant(name, &variant.rust_name));
                    for field in &variant.fields {
                        if let Some(field) = &field.rust_name {
                            output.insert(Member::variant_field(name, &variant.rust_name, field));
                        } else if (tagged_values
                            && !matches!(&field.ty, Type::Tuple(tuple) if tuple.elems.is_empty()))
                            || serializes_as_variant_value(schema, &field.ty)
                        {
                            output.insert(Member::variant_field(name, &variant.rust_name, "value"));
                        }
                    }
                }
            }
        }
    }
    output
}

struct MappingRow<'a> {
    owner: &'a str,
    domain: &'a str,
    path: &'a str,
    owner_class: &'a str,
}

fn mapping_rows() -> Vec<MappingRow<'static>> {
    canonical_projection_owner_manifest()
        .lines()
        .skip(1)
        .map(|line| {
            let columns = line.split('|').collect::<Vec<_>>();
            MappingRow {
                owner: columns[0],
                domain: columns[1],
                path: columns[2],
                owner_class: columns[3],
            }
        })
        .collect()
}

#[test]
fn schema_generated_owner_to_path_mapping_is_exact_and_bidirectional() {
    let rows = mapping_rows();
    let normalized = normalized_schema();
    let material = material_schema();
    let owner_expressions = rows
        .iter()
        .filter(|row| row.owner_class == "normalized")
        .map(|row| row.owner)
        .collect::<Vec<_>>();
    let material_expressions = rows.iter().map(|row| row.path).collect::<Vec<_>>();
    let normalized_roots = structural_roots(&normalized, &owner_expressions);
    let material_roots = structural_roots(&material, &material_expressions);

    let mut exact_pairs = BTreeSet::new();
    let mut normalized_members = BTreeSet::new();
    let mut material_members = BTreeSet::new();
    for row in &rows {
        let (_material_member, material_trace) =
            resolve_expression(&material, &material_roots, row.path)
                .unwrap_or_else(|error| panic!("{error}"));
        material_members.extend(material_trace);
        if row.owner_class == "normalized" {
            let (_normalized_member, normalized_trace) =
                resolve_expression(&normalized, &normalized_roots, row.owner)
                    .unwrap_or_else(|error| panic!("{error}"));
            normalized_members.extend(normalized_trace);
            assert!(exact_pairs.insert((row.owner, row.domain, row.path)));
        }
    }

    let normalized_definitions = normalized_members
        .iter()
        .map(|member| member.definition.clone())
        .collect::<BTreeSet<_>>();
    let material_definitions = material_members
        .iter()
        .map(|member| member.definition.clone())
        .collect::<BTreeSet<_>>();
    let declared_normalized = declared_members(&normalized, &normalized_definitions, false);
    let mut declared_normalized = declared_normalized;
    declared_normalized.extend(
        normalized_members
            .iter()
            .filter(|member| {
                normalized
                    .extension_fields
                    .get(&member.definition)
                    .is_some_and(|fields| fields.contains(&member.member))
            })
            .cloned(),
    );
    let missing_normalized = declared_normalized
        .difference(&normalized_members)
        .collect::<Vec<_>>();
    let stale_normalized = normalized_members
        .difference(&declared_normalized)
        .collect::<Vec<_>>();
    assert!(
        missing_normalized.is_empty() && stale_normalized.is_empty(),
        "normalized declaration members and mapped owners diverged\nmissing: {missing_normalized:#?}\nstale: {stale_normalized:#?}"
    );

    let declared_material = declared_members(&material, &material_definitions, true);
    let missing_material = declared_material
        .difference(&material_members)
        .collect::<Vec<_>>();
    let stale_material = material_members
        .difference(&declared_material)
        .collect::<Vec<_>>();
    assert!(
        missing_material.is_empty() && stale_material.is_empty(),
        "generated material declarations and mapped paths diverged\nmissing: {missing_material:#?}\nstale: {stale_material:#?}"
    );
}

#[test]
fn projection_schema_macro_is_the_only_material_declaration_source() {
    let implementation = include_str!("../src/canonical.rs");
    assert!(implementation.contains("macro_rules! projection_schema"));
    let generated = collect_schema(CANONICAL_PROJECTION_SCHEMA_DECLARATIONS);
    assert!(
        generated
            .definitions
            .values()
            .any(|definition| definition.public),
        "projection schema emitted no public hash-domain root"
    );

    let prefix = implementation
        .split("projection_schema! {")
        .next()
        .expect("projection schema invocation exists");
    let suffix = implementation
        .split_once("\nfn deserialize_value_contract_material")
        .expect("projection declarations precede their decoders")
        .1;
    assert!(
        !prefix.lines().any(|line| {
            let line = line.trim_start();
            (line.starts_with("struct ")
                || line.starts_with("pub struct ")
                || line.starts_with("enum "))
                && (line.contains("Material") || line.contains("Projection"))
        }),
        "closed material declaration escaped the schema"
    );
    assert!(
        !suffix.lines().any(|line| {
            let line = line.trim_start();
            (line.starts_with("struct ") || line.starts_with("pub struct "))
                && line.contains("MaterialV1")
        }),
        "closed material declaration escaped the schema"
    );
}
