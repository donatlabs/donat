use std::collections::{BTreeMap, BTreeSet};

use donat_connector_catalog::{
    CANONICAL_PROJECTION_MUTATION_DESCRIPTORS, CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS,
    CANONICAL_PROJECTION_SCHEMA_DECLARATIONS, CanonicalDeclarationSource,
    CanonicalMutationDisposition, canonical_projection_owner_manifest,
};
use syn::{
    Attribute, Fields, FnArg, GenericArgument, Item, LitStr, Pat, PathArguments, Type, TypePath,
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

#[test]
fn projection_macro_emits_typed_owner_paths_and_mutation_routes() {
    assert!(
        !CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS.is_empty(),
        "the declaration macro emitted no typed owner/path rows"
    );
    assert!(
        !CANONICAL_PROJECTION_MUTATION_DESCRIPTORS.is_empty(),
        "the declaration macro emitted no mutation routes"
    );
    let owners = CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
        .iter()
        .map(|descriptor| {
            (
                descriptor.domain,
                descriptor.canonical_path,
                descriptor.material_member,
                descriptor.material_source,
            )
        })
        .collect::<BTreeSet<_>>();
    let routes = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .map(|descriptor| {
            (
                descriptor.domain,
                descriptor.canonical_path,
                descriptor.material_member,
                descriptor.material_source,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        owners.len(),
        CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS.len(),
        "duplicate owner/path descriptors are forbidden"
    );
    assert_eq!(
        routes.len(),
        CANONICAL_PROJECTION_MUTATION_DESCRIPTORS.len(),
        "duplicate mutation routes are forbidden"
    );
    assert_eq!(
        owners, routes,
        "owner/path and mutation routes are not bijective"
    );
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
    schema
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

fn material_schema() -> Schema {
    let mut schema = normalized_schema();
    merge_schema(
        &mut schema,
        collect_schema(CANONICAL_PROJECTION_SCHEMA_DECLARATIONS),
    );
    schema
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
    expression: &str,
    declared_terminal_owner: &str,
) -> Result<(Member, BTreeSet<Member>), String> {
    let (logical_root, segments) = tokenize(expression);
    let root = if schema.definitions.contains_key(&logical_root) {
        logical_root
    } else {
        declared_terminal_owner.to_owned()
    };
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

fn public_declaration_closure(schema: &Schema) -> BTreeSet<String> {
    let mut closure = schema
        .definitions
        .iter()
        .filter_map(|(name, definition)| definition.public.then_some(name.clone()))
        .collect::<BTreeSet<_>>();
    loop {
        let mut discovered = BTreeSet::new();
        for name in &closure {
            let Some(definition) = schema.definitions.get(name) else {
                continue;
            };
            let fields = match &definition.shape {
                Shape::Struct(fields) => fields.iter().collect::<Vec<_>>(),
                Shape::Enum(variants) => variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter())
                    .collect::<Vec<_>>(),
            };
            for field in fields {
                if let Some(name) = type_name(&field.ty)
                    && schema.definitions.contains_key(&name)
                {
                    discovered.insert(name);
                }
            }
        }
        let before = closure.len();
        closure.extend(discovered);
        if closure.len() == before {
            return closure;
        }
    }
}

struct MappingRow<'a> {
    owner: &'a str,
    normalized_member: &'a str,
    normalized_source: CanonicalDeclarationSource,
    owner_class: &'a str,
    path: &'a str,
    material_member: &'a str,
    material_source: CanonicalDeclarationSource,
}

fn mapping_rows() -> Vec<MappingRow<'static>> {
    CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
        .iter()
        .map(|descriptor| MappingRow {
            owner: descriptor.normalized_owner,
            normalized_member: descriptor.normalized_member,
            normalized_source: descriptor.normalized_source,
            owner_class: descriptor.owner_class,
            path: descriptor.canonical_path,
            material_member: descriptor.material_member,
            material_source: descriptor.material_source,
        })
        .collect()
}

fn identity_owner(identity: &str) -> &str {
    identity
        .split(['.', ':'])
        .next()
        .expect("exact member identity has a declaration owner")
}

fn declared_identity(schema: &Schema, identity: &str) -> Result<Member, String> {
    let owner = identity_owner(identity);
    let definition = schema
        .definitions
        .get(owner)
        .ok_or_else(|| format!("{identity}: declaration source has no {owner}"))?;
    if let Some(rest) = identity.strip_prefix(&format!("{owner}::")) {
        let (variant_name, field_name) = rest
            .split_once('.')
            .map_or((rest, None), |(variant, field)| (variant, Some(field)));
        let Shape::Enum(variants) = &definition.shape else {
            return Err(format!("{identity}: {owner} is not an enum"));
        };
        let variant = variants
            .iter()
            .find(|variant| variant.rust_name == variant_name)
            .ok_or_else(|| format!("{identity}: enum variant is not declared"))?;
        if let Some(field_name) = field_name {
            let declared = variant.fields.iter().any(|field| {
                field.rust_name.as_deref() == Some(field_name)
                    || (field.rust_name.is_none() && field_name == "value")
            });
            if !declared {
                return Err(format!("{identity}: enum payload member is not declared"));
            }
            Ok(Member::variant_field(owner, variant_name, field_name))
        } else {
            Ok(Member::variant(owner, variant_name))
        }
    } else {
        let field_name = identity
            .strip_prefix(&format!("{owner}."))
            .ok_or_else(|| format!("{identity}: malformed struct member identity"))?;
        let Shape::Struct(fields) = &definition.shape else {
            return Err(format!("{identity}: {owner} is not a struct"));
        };
        fields
            .iter()
            .any(|field| field.rust_name.as_deref() == Some(field_name))
            .then(|| Member::field(owner, field_name))
            .ok_or_else(|| format!("{identity}: struct field is not declared"))
    }
}

fn builder_parameter_is_declared(identity: &str) -> bool {
    let Some((function_name, parameter_name)) = identity.split_once("::") else {
        return false;
    };
    syn::parse_file(include_str!("../src/canonical.rs"))
        .unwrap()
        .items
        .into_iter()
        .any(|item| {
            let Item::Fn(function) = item else {
                return false;
            };
            function.sig.ident == function_name
                && matches!(function.vis, Visibility::Public(_))
                && function.sig.inputs.iter().any(|argument| {
                    matches!(
                        argument,
                        FnArg::Typed(argument)
                            if matches!(
                                argument.pat.as_ref(),
                                Pat::Ident(name) if name.ident == parameter_name
                            )
                    )
                })
        })
}

#[test]
fn macro_owner_paths_are_the_exact_independent_adr_set() {
    let implementation = CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
        .iter()
        .map(|descriptor| {
            (
                descriptor.normalized_owner,
                descriptor.domain,
                descriptor.canonical_path,
                descriptor.owner_class,
                descriptor.order,
                descriptor.null_empty,
                descriptor.branch_type,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        implementation.len(),
        CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS.len(),
        "the macro emitted a duplicate owner/path descriptor"
    );
    let adr = canonical_projection_owner_manifest()
        .lines()
        .skip(1)
        .map(|line| {
            let columns = line.split('|').collect::<Vec<_>>();
            (
                columns[0], columns[1], columns[2], columns[3], columns[4], columns[5], columns[6],
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(implementation, adr, "macro inventory and ADR 012 diverged");
}

#[test]
fn schema_generated_owner_to_path_mapping_is_exact_and_bidirectional() {
    let rows = mapping_rows();
    let source = collect_schema(include_str!("../src/source.rs"));
    let model = collect_schema(include_str!("../src/model.rs"));
    let value_contract = collect_schema(include_str!("../../value-contract/src/lib.rs"));
    let mut connector_abi = collect_schema(include_str!("../../connector-abi/src/lib.rs"));
    merge_schema(
        &mut connector_abi,
        collect_schema(include_str!("../../connector-abi/src/envelope.rs")),
    );
    let projection = collect_schema(CANONICAL_PROJECTION_SCHEMA_DECLARATIONS);
    let normalized = normalized_schema();
    let material = material_schema();
    let external_schema = |source_kind| match source_kind {
        CanonicalDeclarationSource::Source => &source,
        CanonicalDeclarationSource::Model => &model,
        CanonicalDeclarationSource::ValueContract => &value_contract,
        CanonicalDeclarationSource::ConnectorAbi => &connector_abi,
        other => panic!("expected an external declaration source, got {other:?}"),
    };

    let mut normalized_members = BTreeSet::new();
    for row in &rows {
        match row.normalized_source {
            CanonicalDeclarationSource::Source
            | CanonicalDeclarationSource::Model
            | CanonicalDeclarationSource::ValueContract
            | CanonicalDeclarationSource::ConnectorAbi => {
                assert_eq!(row.owner_class, "normalized");
                let declared = declared_identity(
                    external_schema(row.normalized_source),
                    row.normalized_member,
                )
                .unwrap_or_else(|error| panic!("{error}"));
                let terminal_owner = identity_owner(row.normalized_member);
                let (resolved, trace) = resolve_expression(&normalized, row.owner, terminal_owner)
                    .unwrap_or_else(|error| panic!("{error}"));
                assert_eq!(
                    declared, resolved,
                    "normalized owner expression and exact generated member diverged: {}",
                    row.owner
                );
                normalized_members.extend(trace);
            }
            CanonicalDeclarationSource::BuilderDerived => {
                assert_eq!(row.owner_class, "normalized");
                assert!(
                    builder_parameter_is_declared(row.normalized_member),
                    "generated builder-derived member is not an exact public parameter: {}",
                    row.normalized_member
                );
            }
            CanonicalDeclarationSource::Constant => {
                assert_eq!(row.owner_class, "constant");
                assert_eq!(row.normalized_member, row.owner);
            }
            CanonicalDeclarationSource::NamedDerived => {
                assert!(row.owner_class.starts_with("derived:"));
                assert_eq!(row.normalized_member, row.owner);
            }
            CanonicalDeclarationSource::ProjectionSchema => {
                panic!("normalized owner cannot be declared by the material schema")
            }
        }
    }

    let normalized_definitions = normalized_members
        .iter()
        .map(|member| member.definition.clone())
        .collect::<BTreeSet<_>>();
    let declared_normalized = declared_members(&normalized, &normalized_definitions, false);
    let missing_normalized = declared_normalized
        .difference(&normalized_members)
        .collect::<Vec<_>>();
    let stale_normalized = normalized_members
        .difference(&declared_normalized)
        .collect::<Vec<_>>();
    assert!(
        missing_normalized.is_empty() && stale_normalized.is_empty(),
        "normalized declarations and exact generated members diverged\nmissing: {missing_normalized:#?}\nstale: {stale_normalized:#?}"
    );

    let mut material_members = BTreeSet::new();
    for row in &rows {
        let declared_schema = match row.material_source {
            CanonicalDeclarationSource::ProjectionSchema => &projection,
            CanonicalDeclarationSource::Source
            | CanonicalDeclarationSource::Model
            | CanonicalDeclarationSource::ValueContract
            | CanonicalDeclarationSource::ConnectorAbi => external_schema(row.material_source),
            other => panic!("material member has invalid declaration source {other:?}"),
        };
        let declared = declared_identity(declared_schema, row.material_member)
            .unwrap_or_else(|error| panic!("{error}"));
        let declared_terminal_owner = identity_owner(row.material_member);
        let (material_member, material_trace) =
            resolve_expression(&material, row.path, declared_terminal_owner)
                .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            material_member, declared,
            "generated route is not tied to its exact declaration member"
        );
        material_members.extend(material_trace);
    }

    for descriptor in CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .filter(|descriptor| descriptor.disposition == CanonicalMutationDisposition::Singleton)
    {
        let (owner, field) = descriptor
            .material_member
            .split_once('.')
            .expect("singleton descriptor owns one exact struct field");
        let Some(Definition {
            shape: Shape::Struct(fields),
            ..
        }) = material.definitions.get(owner)
        else {
            panic!("singleton owner declaration is missing: {owner}");
        };
        let field = fields
            .iter()
            .find(|candidate| candidate.rust_name.as_deref() == Some(field))
            .expect("singleton field declaration is missing");
        let singleton = type_name(&field.ty).expect("singleton field has a named enum type");
        let Some(Definition {
            shape: Shape::Enum(variants),
            ..
        }) = material.definitions.get(&singleton)
        else {
            panic!("singleton branch declaration is missing: {singleton}");
        };
        assert_eq!(
            variants.len(),
            1,
            "generated singleton disposition names a non-singleton enum"
        );
        material_members.insert(Member::variant(&singleton, &variants[0].rust_name));
    }

    let mut material_definitions = public_declaration_closure(&projection);
    material_definitions.extend(
        material_members
            .iter()
            .map(|member| member.definition.clone()),
    );
    material_definitions.extend(
        rows.iter()
            .map(|row| identity_owner(row.material_member).to_owned()),
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
