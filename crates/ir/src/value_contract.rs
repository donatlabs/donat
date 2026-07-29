use std::collections::{BTreeMap, BTreeSet};

use donat_metadata::Metadata;
use donat_value_contract::{
    TypeRef, ValueContractCatalog, ValueContractError, ValueContractField, ValueObjectContract,
    ValueScalar, ValueType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStartPolicy {
    Enabled,
    RejectRetired,
}

enum NamedType<'a> {
    Input(&'a donat_metadata::InputObjectType),
    Enum(&'a donat_metadata::EnumType),
    Scalar,
}

pub fn compile_value_contract_catalog(
    metadata: &Metadata,
    fields: &BTreeMap<String, String>,
) -> Result<ValueContractCatalog, ValueContractError> {
    let mut named = BTreeMap::new();
    for input in &metadata.custom_types.input_objects {
        insert_named(&mut named, &input.name, NamedType::Input(input))?;
    }
    for enum_ in &metadata.custom_types.enums {
        insert_named(&mut named, &enum_.name, NamedType::Enum(enum_))?;
    }
    for scalar in &metadata.custom_types.scalars {
        insert_named(&mut named, &scalar.name, NamedType::Scalar)?;
    }

    let mut roots = BTreeMap::new();
    for (name, source) in fields {
        if !valid_name(name) {
            return Err(ValueContractError::InvalidTypeRef(format!(
                "invalid value-contract field name `{name}`"
            )));
        }
        roots.insert(
            name.clone(),
            ValueContractField {
                required: source.ends_with('!'),
                type_ref: resolve_type(TypeRef::parse(source)?, &named)?,
            },
        );
    }

    let mut named_objects = BTreeMap::new();
    for (name, kind) in &named {
        let NamedType::Input(input) = kind else {
            continue;
        };
        let mut object_fields = BTreeMap::new();
        for field in &input.fields {
            if !valid_name(&field.name) {
                return Err(ValueContractError::InvalidTypeRef(format!(
                    "invalid field `{}` in input object `{name}`",
                    field.name
                )));
            }
            if object_fields
                .insert(
                    field.name.clone(),
                    ValueContractField {
                        required: field.type_.ends_with('!'),
                        type_ref: resolve_type(TypeRef::parse(&field.type_)?, &named)?,
                    },
                )
                .is_some()
            {
                return Err(ValueContractError::InvalidTypeRef(format!(
                    "duplicate field `{}` in input object `{name}`",
                    field.name
                )));
            }
        }
        named_objects.insert(
            (*name).to_owned(),
            ValueObjectContract {
                fields: object_fields,
            },
        );
    }

    Ok(ValueContractCatalog {
        roots,
        named_objects,
    })
}

fn insert_named<'a>(
    named: &mut BTreeMap<&'a str, NamedType<'a>>,
    name: &'a str,
    kind: NamedType<'a>,
) -> Result<(), ValueContractError> {
    if !valid_name(name) || builtin_name(name) {
        return Err(ValueContractError::InvalidTypeRef(format!(
            "invalid custom type name `{name}`"
        )));
    }
    if named.insert(name, kind).is_some() {
        return Err(ValueContractError::InvalidTypeRef(format!(
            "duplicate custom type name `{name}`"
        )));
    }
    Ok(())
}

fn resolve_type(
    mut type_ref: TypeRef,
    named: &BTreeMap<&str, NamedType<'_>>,
) -> Result<TypeRef, ValueContractError> {
    let mut pending = vec![&mut type_ref];
    while let Some(type_ref) = pending.pop() {
        if let ValueType::Ref { name } = &type_ref.value_type {
            let name = name.clone();
            type_ref.value_type = match named.get(name.as_str()) {
                Some(NamedType::Input(_)) => ValueType::Ref { name },
                Some(NamedType::Enum(enum_)) => {
                    let values = enum_
                        .values
                        .iter()
                        .map(|value| value.value.clone())
                        .collect::<Vec<_>>();
                    if values.is_empty()
                        || values.iter().collect::<BTreeSet<_>>().len() != values.len()
                    {
                        return Err(ValueContractError::InvalidTypeRef(format!(
                            "enum `{name}` must contain unique values"
                        )));
                    }
                    ValueType::Enum { name, values }
                }
                Some(NamedType::Scalar) => ValueType::Scalar {
                    scalar: ValueScalar::Custom { name },
                },
                None => {
                    return Err(ValueContractError::InvalidTypeRef(format!(
                        "unknown named type `{name}`"
                    )));
                }
            };
            continue;
        }
        match &mut type_ref.value_type {
            ValueType::List { element } => pending.push(element),
            ValueType::Object { fields } => {
                pending.extend(fields.values_mut().map(|field| &mut field.type_ref));
            }
            ValueType::Scalar { .. } | ValueType::Enum { .. } | ValueType::Ref { .. } => {}
        }
    }
    Ok(type_ref)
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn builtin_name(name: &str) -> bool {
    matches!(
        name,
        "Boolean"
            | "bool"
            | "boolean"
            | "String"
            | "string"
            | "ID"
            | "Int"
            | "int"
            | "int32"
            | "int64"
            | "uint64"
            | "Float"
            | "float"
            | "decimal"
            | "uuid"
            | "date"
            | "timestamp"
            | "timestamptz"
            | "json"
            | "jsonb"
    )
}
