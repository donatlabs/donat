use std::collections::{BTreeMap, BTreeSet, VecDeque};

use donat_connector_catalog::canonical_projection_owner_manifest;
use syn::{
    Attribute, Fields, GenericArgument, Item, LitStr, PathArguments, Type, TypeArray, TypeGroup,
    TypeParen, TypePath, TypeReference, TypeSlice, TypeTuple,
};

#[derive(Clone)]
enum Definition {
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
}

#[derive(Clone)]
struct Field {
    name: Option<String>,
    wire_name: Option<String>,
    dependencies: Vec<String>,
}

#[derive(Clone)]
struct Variant {
    name: String,
    wire_name: String,
    fields: Vec<Field>,
}

fn serde_rename(attributes: &[Attribute]) -> Option<String> {
    let mut rename = None;
    for attribute in attributes {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value = meta.value()?;
                rename = Some(value.parse::<LitStr>()?.value());
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
        let previous = index.checked_sub(1).and_then(|index| characters.get(index));
        let next = characters.get(index + 1);
        if character.is_ascii_uppercase()
            && index > 0
            && (previous.is_some_and(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
                || next.is_some_and(|value| value.is_ascii_lowercase()))
        {
            output.push('_');
        }
        output.push(character.to_ascii_lowercase());
    }
    output
}

fn type_dependencies(value: &Type, output: &mut Vec<String>) {
    match value {
        Type::Array(TypeArray { elem, .. })
        | Type::Group(TypeGroup { elem, .. })
        | Type::Paren(TypeParen { elem, .. })
        | Type::Reference(TypeReference { elem, .. })
        | Type::Slice(TypeSlice { elem, .. }) => type_dependencies(elem, output),
        Type::Path(TypePath { path, .. }) => {
            for segment in &path.segments {
                output.push(segment.ident.to_string());
                if let PathArguments::AngleBracketed(arguments) = &segment.arguments {
                    for argument in &arguments.args {
                        if let GenericArgument::Type(value) = argument {
                            type_dependencies(value, output);
                        }
                    }
                }
            }
        }
        Type::Tuple(TypeTuple { elems, .. }) => {
            for value in elems {
                type_dependencies(value, output);
            }
        }
        _ => {}
    }
}

fn field(name: Option<String>, attributes: &[Attribute], value: &Type) -> Field {
    let mut dependencies = Vec::new();
    type_dependencies(value, &mut dependencies);
    let wire_name = serde_rename(attributes).or_else(|| name.clone());
    Field {
        name,
        wire_name,
        dependencies,
    }
}

fn collect_definitions(source: &str, definitions: &mut BTreeMap<String, Definition>) {
    let file = syn::parse_file(source).unwrap();
    for item in file.items {
        match item {
            Item::Struct(value) => {
                let fields = match value.fields {
                    Fields::Named(fields) => fields
                        .named
                        .into_iter()
                        .map(|value| {
                            field(
                                value.ident.map(|name| name.to_string()),
                                &value.attrs,
                                &value.ty,
                            )
                        })
                        .collect(),
                    Fields::Unnamed(fields) => fields
                        .unnamed
                        .into_iter()
                        .map(|value| field(None, &value.attrs, &value.ty))
                        .collect(),
                    Fields::Unit => Vec::new(),
                };
                definitions.insert(value.ident.to_string(), Definition::Struct(fields));
            }
            Item::Enum(value) => {
                let variants = value
                    .variants
                    .into_iter()
                    .map(|variant| {
                        let name = variant.ident.to_string();
                        Variant {
                            wire_name: serde_rename(&variant.attrs)
                                .unwrap_or_else(|| snake_case(&name)),
                            name,
                            fields: match variant.fields {
                                Fields::Named(fields) => fields
                                    .named
                                    .into_iter()
                                    .map(|value| {
                                        field(
                                            value.ident.map(|name| name.to_string()),
                                            &value.attrs,
                                            &value.ty,
                                        )
                                    })
                                    .collect(),
                                Fields::Unnamed(fields) => fields
                                    .unnamed
                                    .into_iter()
                                    .map(|value| field(None, &value.attrs, &value.ty))
                                    .collect(),
                                Fields::Unit => Vec::new(),
                            },
                        }
                    })
                    .collect();
                definitions.insert(value.ident.to_string(), Definition::Enum(variants));
            }
            _ => {}
        }
    }
}

fn implementation_inventory() -> BTreeSet<String> {
    let mut definitions = BTreeMap::new();
    for source in [
        include_str!("../src/source.rs"),
        include_str!("../src/model.rs"),
        include_str!("../../value-contract/src/lib.rs"),
        include_str!("../../connector-abi/src/lib.rs"),
        include_str!("../../connector-abi/src/envelope.rs"),
    ] {
        collect_definitions(source, &mut definitions);
    }

    let mut inventory = BTreeSet::new();
    let mut pending = VecDeque::from([
        "ConnectorSourceRecord".to_owned(),
        "ConnectorManifest".to_owned(),
        "ValueContractCatalog".to_owned(),
        "TypedValue".to_owned(),
        "ConnectorErrorClass".to_owned(),
    ]);
    let mut visited = BTreeSet::new();
    while let Some(owner) = pending.pop_front() {
        if !visited.insert(owner.clone()) {
            continue;
        }
        if matches!(
            owner.as_str(),
            "TypedValueMaterialV1" | "TypedValueMaterial" | "StaticSafeMessage"
        ) {
            continue;
        }
        if owner == "TypedValue" {
            inventory.extend(
                [
                    "TypedValue::Null",
                    "TypedValue::Boolean",
                    "TypedValue::Boolean.value",
                    "TypedValue::String",
                    "TypedValue::String.value",
                    "TypedValue::I64",
                    "TypedValue::I64.value",
                    "TypedValue::U64",
                    "TypedValue::U64.value",
                    "TypedValue::Decimal",
                    "TypedValue::Decimal.value",
                    "TypedValue::List",
                    "TypedValue::List.value",
                    "TypedValue::Object",
                    "TypedValue::Object.value",
                    "TypedValue::InlineBytes",
                    "TypedValue::InlineBytes.bytes",
                    "TypedValue::InlineBytes.media_type",
                    "TypedValue::InlineBytes.file_name",
                ]
                .into_iter()
                .map(str::to_owned),
            );
            continue;
        }
        let Some(definition) = definitions.get(&owner) else {
            continue;
        };
        match definition {
            Definition::Struct(fields) => {
                for field in fields {
                    if let Some(name) = &field.name {
                        inventory.insert(format!("{owner}.{name}"));
                    }
                    pending.extend(field.dependencies.iter().cloned());
                }
            }
            Definition::Enum(variants) => {
                for variant in variants {
                    inventory.insert(format!("{owner}::{}", variant.name));
                    for field in &variant.fields {
                        if let Some(name) = &field.name {
                            inventory.insert(format!("{owner}::{}.{name}", variant.name));
                        }
                        pending.extend(field.dependencies.iter().cloned());
                    }
                }
            }
        }
    }
    inventory.insert("NpmIntegrity.algorithm".to_owned());
    inventory.insert("ValueContractCatalog.value_language_epoch".to_owned());
    inventory
        .into_iter()
        .map(|owner| {
            owner
                .replace("ValueContractField.", "Field.")
                .replace("ValueObjectContract.", "NamedObject.")
        })
        .collect()
}

fn recursively_expanded_inventory() -> BTreeSet<String> {
    let mut definitions = BTreeMap::new();
    for source in [
        include_str!("../src/source.rs"),
        include_str!("../src/model.rs"),
        include_str!("../../value-contract/src/lib.rs"),
        include_str!("../../connector-abi/src/lib.rs"),
        include_str!("../../connector-abi/src/envelope.rs"),
    ] {
        collect_definitions(source, &mut definitions);
    }

    fn expand(
        definitions: &BTreeMap<String, Definition>,
        owner: &str,
        prefix: &str,
        depth: usize,
        output: &mut BTreeSet<String>,
    ) {
        if depth > 8
            || matches!(
                owner,
                "TypedValue"
                    | "TypedValueMaterial"
                    | "TypedValueMaterialV1"
                    | "BoundedInlineBytes"
                    | "CanonicalNumber"
            )
        {
            return;
        }
        let Some(definition) = definitions.get(owner) else {
            return;
        };
        match definition {
            Definition::Struct(fields) => {
                for field in fields {
                    let Some(name) = &field.name else {
                        continue;
                    };
                    for separator in [".", "[]."] {
                        let child_prefix = format!("{prefix}{separator}{name}");
                        output.insert(child_prefix.clone());
                        for dependency in &field.dependencies {
                            if definitions.contains_key(dependency) {
                                expand(definitions, dependency, &child_prefix, depth + 1, output);
                            }
                        }
                    }
                }
            }
            Definition::Enum(variants) => {
                for variant in variants {
                    let variant_prefix = format!("{prefix}::{}", variant.name);
                    output.insert(variant_prefix.clone());
                    for field in &variant.fields {
                        if let Some(name) = &field.name {
                            let child_prefix = format!("{variant_prefix}.{name}");
                            output.insert(child_prefix.clone());
                            for dependency in &field.dependencies {
                                if definitions.contains_key(dependency) {
                                    expand(
                                        definitions,
                                        dependency,
                                        &child_prefix,
                                        depth + 1,
                                        output,
                                    );
                                }
                            }
                        } else {
                            for dependency in &field.dependencies {
                                if definitions.contains_key(dependency) {
                                    expand(
                                        definitions,
                                        dependency,
                                        &variant_prefix,
                                        depth + 1,
                                        output,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut output = BTreeSet::new();
    for root in definitions.keys() {
        expand(&definitions, root, root, 0, &mut output);
    }
    output
        .into_iter()
        .map(|owner| {
            owner
                .replace("ValueContractField.", "Field.")
                .replace("ValueObjectContract.", "NamedObject.")
        })
        .collect()
}

fn manifest_inventory() -> BTreeSet<String> {
    canonical_projection_owner_manifest()
        .lines()
        .skip(1)
        .filter_map(|line| {
            let columns = line.split('|').collect::<Vec<_>>();
            (columns[3] == "normalized").then(|| columns[0].to_owned())
        })
        .collect()
}

fn canonical_material_inventory() -> BTreeMap<String, BTreeSet<String>> {
    let mut definitions = BTreeMap::new();
    collect_definitions(include_str!("../src/canonical.rs"), &mut definitions);
    collect_definitions(include_str!("../src/source.rs"), &mut definitions);
    collect_definitions(include_str!("../src/model.rs"), &mut definitions);
    let mut inline_payloads = BTreeSet::new();
    for definition in definitions.values() {
        if let Definition::Enum(variants) = definition {
            for variant in variants {
                if variant.fields.len() == 1 && variant.fields[0].name.is_none() {
                    for dependency in &variant.fields[0].dependencies {
                        if definitions.contains_key(dependency) {
                            inline_payloads.insert(dependency.clone());
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        definitions: &BTreeMap<String, Definition>,
        inline_payloads: &BTreeSet<String>,
        definition_name: &str,
        owner_name: &str,
        identity_name: &str,
        depth: usize,
        visited: &mut BTreeSet<(String, String, String)>,
        output: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        if depth > 8 {
            return;
        }
        if !visited.insert((
            definition_name.to_owned(),
            owner_name.to_owned(),
            identity_name.to_owned(),
        )) {
            return;
        }
        let Some(definition) = definitions.get(definition_name) else {
            return;
        };
        match definition {
            Definition::Struct(fields) => {
                if fields.len() == 1 && fields[0].name.is_none() {
                    for dependency in &fields[0].dependencies {
                        if definitions.contains_key(dependency) {
                            visit(
                                definitions,
                                inline_payloads,
                                dependency,
                                owner_name,
                                identity_name,
                                depth + 1,
                                visited,
                                output,
                            );
                        }
                    }
                    return;
                }
                for field in fields {
                    let Some(name) = &field.wire_name else {
                        continue;
                    };
                    output
                        .entry(format!("{identity_name}.{name}"))
                        .or_default()
                        .insert(format!("{owner_name}.{name}"));
                    for dependency in &field.dependencies {
                        if definitions.contains_key(dependency)
                            && !inline_payloads.contains(dependency)
                        {
                            visit(
                                definitions,
                                inline_payloads,
                                dependency,
                                dependency,
                                dependency,
                                depth + 1,
                                visited,
                                output,
                            );
                            for nested_owner in [
                                format!("{owner_name}.{name}"),
                                format!("{owner_name}.{name}[]"),
                            ] {
                                visit(
                                    definitions,
                                    inline_payloads,
                                    dependency,
                                    &nested_owner,
                                    dependency,
                                    depth + 1,
                                    visited,
                                    output,
                                );
                            }
                        }
                    }
                }
            }
            Definition::Enum(variants) => {
                for variant in variants {
                    let variant_owner = format!("{owner_name}{{kind={}}}", variant.wire_name);
                    output
                        .entry(format!(
                            "{identity_name}{{kind={}}}.kind",
                            variant.wire_name
                        ))
                        .or_default()
                        .insert(format!("{variant_owner}.kind"));
                    for field in &variant.fields {
                        if let Some(name) = &field.wire_name {
                            output
                                .entry(format!(
                                    "{identity_name}{{kind={}}}.value.{name}",
                                    variant.wire_name
                                ))
                                .or_default()
                                .insert(format!("{variant_owner}.value.{name}"));
                            for dependency in &field.dependencies {
                                if definitions.contains_key(dependency)
                                    && !inline_payloads.contains(dependency)
                                {
                                    visit(
                                        definitions,
                                        inline_payloads,
                                        dependency,
                                        dependency,
                                        dependency,
                                        depth + 1,
                                        visited,
                                        output,
                                    );
                                    visit(
                                        definitions,
                                        inline_payloads,
                                        dependency,
                                        &format!("{variant_owner}.value.{name}"),
                                        dependency,
                                        depth + 1,
                                        visited,
                                        output,
                                    );
                                }
                            }
                        } else {
                            if matches!(definition_name, "TypedValueMaterial" | "ValueTypeMaterial")
                            {
                                output
                                    .entry(format!(
                                        "{identity_name}{{kind={}}}.value",
                                        variant.wire_name
                                    ))
                                    .or_default()
                                    .insert(format!("{variant_owner}.value"));
                            }
                            for dependency in &field.dependencies {
                                let Some(payload) = definitions.get(dependency) else {
                                    continue;
                                };
                                match payload {
                                    Definition::Struct(fields) => {
                                        if fields.len() == 1 && fields[0].name.is_none() {
                                            visit(
                                                definitions,
                                                inline_payloads,
                                                dependency,
                                                &variant_owner,
                                                dependency,
                                                depth + 1,
                                                visited,
                                                output,
                                            );
                                            continue;
                                        }
                                        for field in fields {
                                            let Some(name) = &field.wire_name else {
                                                continue;
                                            };
                                            output
                                                .entry(format!("{dependency}.{name}"))
                                                .or_default()
                                                .insert(format!("{variant_owner}.value.{name}"));
                                            for dependency in &field.dependencies {
                                                if definitions.contains_key(dependency)
                                                    && !inline_payloads.contains(dependency)
                                                {
                                                    visit(
                                                        definitions,
                                                        inline_payloads,
                                                        dependency,
                                                        dependency,
                                                        dependency,
                                                        depth + 1,
                                                        visited,
                                                        output,
                                                    );
                                                    visit(
                                                        definitions,
                                                        inline_payloads,
                                                        dependency,
                                                        &format!("{variant_owner}.value.{name}"),
                                                        dependency,
                                                        depth + 1,
                                                        visited,
                                                        output,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    Definition::Enum(_) => visit(
                                        definitions,
                                        inline_payloads,
                                        dependency,
                                        owner_name,
                                        identity_name,
                                        depth + 1,
                                        visited,
                                        output,
                                    ),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut output = BTreeMap::new();
    let mut visited = BTreeSet::new();
    for root in [
        "SourceRecordMaterialV1",
        "SemanticMaterialV1",
        "ProvenanceMaterialV1",
        "ValueContractMaterialV1",
    ] {
        visit(
            &definitions,
            &inline_payloads,
            root,
            root,
            root,
            0,
            &mut visited,
            &mut output,
        );
    }
    output
}

fn canonical_manifest_inventory() -> BTreeSet<String> {
    canonical_projection_owner_manifest()
        .lines()
        .skip(1)
        .map(|line| line.split('|').nth(2).unwrap().to_owned())
        .collect()
}

#[test]
fn normalized_owner_inventory_is_generated_from_the_real_rust_schema() {
    let implementation = implementation_inventory();
    let expanded = recursively_expanded_inventory();
    let manifest = manifest_inventory();
    let missing = implementation.difference(&manifest).collect::<Vec<_>>();
    let stale = manifest
        .difference(&implementation)
        .filter(|owner| !expanded.contains(*owner))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "missing from ADR: {missing:#?}\nstale in ADR: {stale:#?}"
    );
}

#[test]
fn canonical_member_inventory_is_generated_from_the_material_declarations() {
    let implementation = canonical_material_inventory();
    let manifest = canonical_manifest_inventory();
    let missing = implementation
        .iter()
        .filter(|(member, paths)| {
            if manifest.contains(*member)
                || !paths.is_disjoint(&manifest)
                || paths.iter().any(|path| {
                    manifest.iter().any(|candidate| {
                        candidate.starts_with(&format!("{path}."))
                            || candidate.starts_with(&format!("{path}[]"))
                    })
                })
            {
                return false;
            }
            let adapters = paths
                .iter()
                .flat_map(|path| {
                    [
                        path.replace("ImmutableRepositoryMaterialV1", "ImmutableRepository"),
                        path.replace("NpmIntegrityMaterialV1", "NpmIntegrity"),
                        path.replace("ValueScalarMaterial{", "ValueScalarMaterialV1{"),
                        path.replace("ValueTypeMaterial{", "ValueTypeMaterialV1{"),
                        path.replace(
                            "ResolvedFactOriginMaterialV1.origin{",
                            "ResolvedFactOriginMaterialV1{",
                        ),
                        path.replace("ResolvedFactOriginV1{", "ResolvedFactOriginMaterialV1{"),
                    ]
                })
                .collect::<BTreeSet<_>>();
            if !adapters.is_disjoint(&manifest) {
                return false;
            }
            match member.as_str() {
                "HttpsMaterialV1{kind=https}.kind" => {
                    !manifest.contains("SemanticOriginMaterialV1.scheme")
                }
                "NpmIntegrityAlgorithmMaterialV1{kind=sha512}.kind" => {
                    !manifest.contains("NpmIntegrity.algorithm")
                }
                "ProvenanceConnectorIdentity.id" => {
                    !manifest.contains("SemanticMaterialV1.connector.id")
                }
                "ProvenanceConnectorIdentity.version" => {
                    !manifest.contains("SemanticMaterialV1.connector.version")
                }
                "ProvenanceMaterialV1.artifacts" => !manifest
                    .iter()
                    .any(|path| path.starts_with("ArtifactDecisionMaterialV1.")),
                "ProvenanceMaterialV1.sources" => !manifest
                    .iter()
                    .any(|path| path.starts_with("SourceIdentityMaterialV1.")),
                _ => true,
            }
        })
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "canonical material members missing from ADR: {missing:#?}"
    );
    let implementation_paths = implementation
        .values()
        .flatten()
        .chain(implementation.keys())
        .flat_map(|path| {
            [
                path.clone(),
                path.replace("ImmutableRepositoryMaterialV1", "ImmutableRepository"),
                path.replace("NpmIntegrityMaterialV1", "NpmIntegrity"),
                path.replace("ValueScalarMaterial{", "ValueScalarMaterialV1{"),
                path.replace("ValueTypeMaterial{", "ValueTypeMaterialV1{"),
                path.replace(
                    "ResolvedFactOriginMaterialV1.origin{",
                    "ResolvedFactOriginMaterialV1{",
                ),
                path.replace("ResolvedFactOriginV1{", "ResolvedFactOriginMaterialV1{"),
            ]
        })
        .collect::<BTreeSet<_>>();
    let stale = manifest
        .iter()
        .filter(|candidate| {
            !implementation_paths.iter().any(|path| {
                path == *candidate
                    || candidate.starts_with(&format!("{path}."))
                    || candidate.starts_with(&format!("{path}[]"))
            })
        })
        .collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "ADR canonical paths absent from the material declarations: {stale:#?}"
    );
}
