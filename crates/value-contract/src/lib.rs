#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

pub const VALUE_TYPE_LANGUAGE_VERSION: u16 = 1;
const MAXIMUM_DECODED_INLINE_BYTES: usize = 131_072;
const MAXIMUM_INLINE_VALUES: usize = 16;
const MAXIMUM_CANONICAL_BYTES: usize = 262_144;
const MAXIMUM_MEDIA_TYPE_BYTES: usize = 255;
const MAXIMUM_FILE_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueContractCatalog {
    pub roots: BTreeMap<String, ValueContractField>,
    pub named_objects: BTreeMap<String, ValueObjectContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueContractField {
    pub required: bool,
    pub type_ref: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub nullable: bool,
    pub value_type: ValueType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueScalar {
    Boolean,
    String,
    Int32,
    Int64,
    UInt64,
    Decimal,
    Uuid,
    Date,
    Timestamp,
    TimestampTz,
    Json,
    Custom { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueType {
    Scalar {
        scalar: ValueScalar,
    },
    Enum {
        name: String,
        values: Vec<String>,
    },
    Object {
        fields: BTreeMap<String, ValueContractField>,
    },
    List {
        element: Box<TypeRef>,
    },
    Ref {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueObjectContract {
    pub fields: BTreeMap<String, ValueContractField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalNumber {
    I64(i64),
    U64(u64),
    Decimal(CanonicalDecimal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalDecimal(String);

impl CanonicalDecimal {
    pub fn try_new(value: &str) -> Result<Self, ValueContractError> {
        if valid_canonical_decimal(value) {
            Ok(Self(String::from(value)))
        } else {
            Err(ValueContractError::InvalidValue(String::from(
                "decimal must use minimal fixed-point JSON spelling",
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedValue {
    Null,
    Boolean(bool),
    String(String),
    Number(CanonicalNumber),
    List(Vec<TypedValue>),
    Object(BTreeMap<String, TypedValue>),
    InlineBytes(BoundedInlineBytes),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedInlineBytes {
    bytes: Vec<u8>,
    media_type: BoundedMediaType,
    file_name: Option<BoundedFileName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedMediaType(String);

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedFileName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueContractError {
    InvalidTypeRef(String),
    InvalidValue(String),
    LimitExceeded(&'static str),
    SizeOverflow,
}

impl fmt::Display for ValueContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTypeRef(message) | Self::InvalidValue(message) => {
                formatter.write_str(message)
            }
            Self::LimitExceeded(limit) => write!(formatter, "{limit} limit exceeded"),
            Self::SizeOverflow => formatter.write_str("canonical size overflow"),
        }
    }
}

impl TypeRef {
    pub fn parse(source: &str) -> Result<Self, ValueContractError> {
        fn canonical_named_type(name: &str) -> ValueType {
            let scalar = match name {
                "Boolean" | "bool" | "boolean" => Some(ValueScalar::Boolean),
                "String" | "string" | "ID" => Some(ValueScalar::String),
                "Int" | "int" | "int32" => Some(ValueScalar::Int32),
                "int64" => Some(ValueScalar::Int64),
                "uint64" => Some(ValueScalar::UInt64),
                "Float" | "float" | "decimal" => Some(ValueScalar::Decimal),
                "uuid" => Some(ValueScalar::Uuid),
                "date" => Some(ValueScalar::Date),
                "timestamp" => Some(ValueScalar::Timestamp),
                "timestamptz" => Some(ValueScalar::TimestampTz),
                "json" | "jsonb" => Some(ValueScalar::Json),
                _ => None,
            };
            scalar.map_or_else(
                || ValueType::Ref {
                    name: String::from(name),
                },
                |scalar| ValueType::Scalar { scalar },
            )
        }

        fn error(offset: usize, expectation: &str) -> ValueContractError {
            ValueContractError::InvalidTypeRef(alloc::format!(
                "invalid type reference at byte {offset}: {expectation}"
            ))
        }

        let bytes = source.as_bytes();
        let mut offset = 0;
        while bytes.get(offset) == Some(&b'[') {
            offset += 1;
        }
        let list_depth = offset;
        let start = offset;
        let Some(first) = bytes.get(offset).copied() else {
            return Err(error(offset, "expected a type name"));
        };
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return Err(error(
                offset,
                "type name must begin with an ASCII letter or `_`",
            ));
        }
        offset += 1;
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            offset += 1;
        }
        let mut parsed = TypeRef {
            nullable: true,
            value_type: canonical_named_type(&source[start..offset]),
        };
        if bytes.get(offset) == Some(&b'!') {
            parsed.nullable = false;
            offset += 1;
        }
        for _ in 0..list_depth {
            if bytes.get(offset) != Some(&b']') {
                return Err(error(offset, "expected `]`"));
            }
            offset += 1;
            let nullable = if bytes.get(offset) == Some(&b'!') {
                offset += 1;
                false
            } else {
                true
            };
            parsed = TypeRef {
                nullable,
                value_type: ValueType::List {
                    element: Box::new(parsed),
                },
            };
        }
        if offset != bytes.len() {
            return Err(error(offset, "unexpected trailing input"));
        }
        Ok(parsed)
    }
}

impl BoundedInlineBytes {
    pub fn try_new(
        bytes: Vec<u8>,
        media_type: &str,
        file_name: Option<&str>,
        maximum_decoded_bytes: usize,
    ) -> Result<Self, ValueContractError> {
        if maximum_decoded_bytes > MAXIMUM_DECODED_INLINE_BYTES {
            return Err(ValueContractError::LimitExceeded(
                "declared decoded inline bytes",
            ));
        }
        if bytes.len() > maximum_decoded_bytes {
            return Err(ValueContractError::LimitExceeded("decoded inline bytes"));
        }
        if media_type.len() > MAXIMUM_MEDIA_TYPE_BYTES || !media_type.is_ascii() {
            return Err(ValueContractError::InvalidValue(String::from(
                "media type must contain at most 255 ASCII bytes",
            )));
        }
        if file_name.is_some_and(|name| name.len() > MAXIMUM_FILE_NAME_BYTES) {
            return Err(ValueContractError::InvalidValue(String::from(
                "file name must contain at most 255 UTF-8 bytes",
            )));
        }
        Ok(Self {
            bytes,
            media_type: BoundedMediaType(String::from(media_type)),
            file_name: file_name.map(|name| BoundedFileName(String::from(name))),
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn media_type(&self) -> &str {
        &self.media_type.0
    }

    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_ref().map(|name| name.0.as_str())
    }
}

impl ValueContractCatalog {
    pub fn validate(&self, value: &TypedValue) -> Result<(), ValueContractError> {
        self.validate_definitions()?;
        canonical_size(value)?;
        let TypedValue::Object(values) = value else {
            return Err(ValueContractError::InvalidValue(String::from(
                "a value-contract catalog requires one root object",
            )));
        };
        let mut pending = Vec::new();
        push_fields(values, &self.roots, &mut pending)?;
        while let Some((value, type_ref)) = pending.pop() {
            if matches!(value, TypedValue::Null) {
                if type_ref.nullable {
                    continue;
                }
                return Err(ValueContractError::InvalidValue(String::from(
                    "null is not accepted by a non-null type",
                )));
            }
            match &type_ref.value_type {
                ValueType::Scalar { scalar } => validate_scalar(value, scalar)?,
                ValueType::Enum { name, values } => match value {
                    TypedValue::String(symbol)
                        if values.iter().any(|candidate| candidate == symbol) => {}
                    _ => {
                        return Err(ValueContractError::InvalidValue(alloc::format!(
                            "value does not match enum `{name}`"
                        )));
                    }
                },
                ValueType::Object { fields } => match value {
                    TypedValue::Object(values) => push_fields(values, fields, &mut pending)?,
                    _ => {
                        return Err(ValueContractError::InvalidValue(String::from(
                            "value is not an object",
                        )));
                    }
                },
                ValueType::List { element } => match value {
                    TypedValue::List(values) => {
                        pending.extend(values.iter().map(|value| (value, element.as_ref())));
                    }
                    _ => {
                        return Err(ValueContractError::InvalidValue(String::from(
                            "value is not a list",
                        )));
                    }
                },
                ValueType::Ref { name } => {
                    let object = self.named_objects.get(name).ok_or_else(|| {
                        ValueContractError::InvalidTypeRef(alloc::format!(
                            "unknown named object `{name}`"
                        ))
                    })?;
                    match value {
                        TypedValue::Object(values) => {
                            push_fields(values, &object.fields, &mut pending)?;
                        }
                        _ => {
                            return Err(ValueContractError::InvalidValue(alloc::format!(
                                "value is not object `{name}`"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_definitions(&self) -> Result<(), ValueContractError> {
        let mut pending = self
            .roots
            .values()
            .map(|field| &field.type_ref)
            .collect::<Vec<_>>();
        for object in self.named_objects.values() {
            for field in object.fields.values() {
                pending.push(&field.type_ref);
            }
        }
        while let Some(type_ref) = pending.pop() {
            match &type_ref.value_type {
                ValueType::Scalar { .. } => {}
                ValueType::Enum { name, values } => {
                    if values.is_empty() {
                        return Err(ValueContractError::InvalidTypeRef(alloc::format!(
                            "enum `{name}` must declare at least one value"
                        )));
                    }
                    for (index, value) in values.iter().enumerate() {
                        if values[..index].iter().any(|seen| seen == value) {
                            return Err(ValueContractError::InvalidTypeRef(alloc::format!(
                                "enum `{name}` contains duplicate value `{value}`"
                            )));
                        }
                    }
                }
                ValueType::Object { fields } => {
                    pending.extend(fields.values().map(|field| &field.type_ref));
                }
                ValueType::List { element } => pending.push(element),
                ValueType::Ref { name } => {
                    if !self.named_objects.contains_key(name) {
                        return Err(ValueContractError::InvalidTypeRef(alloc::format!(
                            "unknown named object `{name}`"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_assignable_from(&self, source: &Self) -> bool {
        if self.validate_definitions().is_err() || source.validate_definitions().is_err() {
            return false;
        }
        let mut pending = Vec::new();
        if !fields_assignable(&self.roots, &source.roots, &mut pending) {
            return false;
        }
        if self.named_objects.len() != source.named_objects.len() {
            return false;
        }
        for (name, target_object) in &self.named_objects {
            let Some(source_object) = source.named_objects.get(name) else {
                return false;
            };
            if !fields_assignable(&target_object.fields, &source_object.fields, &mut pending) {
                return false;
            }
        }
        let mut visited_refs = BTreeMap::<(String, String), ()>::new();
        while let Some((target, actual)) = pending.pop() {
            if target.nullable != actual.nullable {
                return false;
            }
            match (&target.value_type, &actual.value_type) {
                (
                    ValueType::Scalar {
                        scalar: ValueScalar::Json,
                    },
                    _,
                ) => {}
                (
                    ValueType::Scalar {
                        scalar: target_scalar,
                    },
                    ValueType::Scalar {
                        scalar: actual_scalar,
                    },
                ) if target_scalar == actual_scalar => {}
                (
                    ValueType::Enum {
                        name: target_name,
                        values: target_values,
                    },
                    ValueType::Enum {
                        name: actual_name,
                        values: actual_values,
                    },
                ) if target_name == actual_name && target_values == actual_values => {}
                (
                    ValueType::List {
                        element: target_element,
                    },
                    ValueType::List {
                        element: actual_element,
                    },
                ) => pending.push((target_element, actual_element)),
                (
                    ValueType::Object {
                        fields: target_fields,
                    },
                    ValueType::Object {
                        fields: actual_fields,
                    },
                ) => {
                    if !fields_assignable(target_fields, actual_fields, &mut pending) {
                        return false;
                    }
                }
                (ValueType::Ref { name: target_name }, ValueType::Ref { name: actual_name })
                    if target_name == actual_name =>
                {
                    let pair = (target_name.clone(), actual_name.clone());
                    if visited_refs.insert(pair, ()).is_none() {
                        let Some(target_object) = self.named_objects.get(target_name) else {
                            return false;
                        };
                        let Some(actual_object) = source.named_objects.get(actual_name) else {
                            return false;
                        };
                        if !fields_assignable(
                            &target_object.fields,
                            &actual_object.fields,
                            &mut pending,
                        ) {
                            return false;
                        }
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

pub fn canonical_size(value: &TypedValue) -> Result<usize, ValueContractError> {
    let mut size = 0_usize;
    let mut inline_values = 0_usize;
    let mut decoded_bytes = 0_usize;
    let mut pending = alloc::vec![value];
    while let Some(value) = pending.pop() {
        match value {
            TypedValue::Null => add_canonical_size(&mut size, 4)?,
            TypedValue::Boolean(true) => add_canonical_size(&mut size, 4)?,
            TypedValue::Boolean(false) => add_canonical_size(&mut size, 5)?,
            TypedValue::String(value) => {
                add_canonical_size(&mut size, jcs_string_size(value)?)?;
            }
            TypedValue::Number(CanonicalNumber::I64(value)) => {
                add_canonical_size(&mut size, signed_decimal_size(*value))?;
            }
            TypedValue::Number(CanonicalNumber::U64(value)) => {
                add_canonical_size(&mut size, unsigned_decimal_size(*value))?;
            }
            TypedValue::Number(CanonicalNumber::Decimal(value)) => {
                add_canonical_size(&mut size, value.as_str().len())?;
            }
            TypedValue::List(values) => {
                add_canonical_size(&mut size, 2)?;
                add_canonical_size(&mut size, values.len().saturating_sub(1))?;
                pending.extend(values);
            }
            TypedValue::Object(values) => {
                add_canonical_size(&mut size, 2)?;
                add_canonical_size(&mut size, values.len().saturating_sub(1))?;
                for (name, value) in values {
                    add_canonical_size(&mut size, jcs_string_size(name)?)?;
                    add_canonical_size(&mut size, 1)?;
                    pending.push(value);
                }
            }
            TypedValue::InlineBytes(value) => {
                inline_values = inline_values
                    .checked_add(1)
                    .ok_or(ValueContractError::SizeOverflow)?;
                if inline_values > MAXIMUM_INLINE_VALUES {
                    return Err(ValueContractError::LimitExceeded("inline value count"));
                }
                decoded_bytes = decoded_bytes
                    .checked_add(value.bytes.len())
                    .ok_or(ValueContractError::SizeOverflow)?;
                if decoded_bytes > MAXIMUM_DECODED_INLINE_BYTES {
                    return Err(ValueContractError::LimitExceeded(
                        "aggregate decoded inline bytes",
                    ));
                }
                add_canonical_size(&mut size, binary_canonical_size(value)?)?;
            }
        }
    }
    Ok(size)
}

fn add_canonical_size(size: &mut usize, additional: usize) -> Result<(), ValueContractError> {
    *size = size
        .checked_add(additional)
        .ok_or(ValueContractError::SizeOverflow)?;
    if *size > MAXIMUM_CANONICAL_BYTES {
        return Err(ValueContractError::LimitExceeded("canonical bytes"));
    }
    Ok(())
}

fn binary_canonical_size(value: &BoundedInlineBytes) -> Result<usize, ValueContractError> {
    let mut size = 2_usize;
    checked_add(&mut size, jcs_string_size("$binary")?)?;
    checked_add(&mut size, 1)?;
    checked_add(&mut size, 2)?;
    checked_add(&mut size, base64url_unpadded_size(value.bytes.len())?)?;
    checked_add(&mut size, 1)?;
    if let Some(file_name) = value.file_name() {
        checked_add(&mut size, jcs_string_size("file_name")?)?;
        checked_add(&mut size, 1)?;
        checked_add(&mut size, jcs_string_size(file_name)?)?;
        checked_add(&mut size, 1)?;
    }
    checked_add(&mut size, jcs_string_size("media_type")?)?;
    checked_add(&mut size, 1)?;
    checked_add(&mut size, jcs_string_size(value.media_type())?)?;
    Ok(size)
}

fn base64url_unpadded_size(decoded: usize) -> Result<usize, ValueContractError> {
    let complete = decoded
        .checked_div(3)
        .and_then(|groups| groups.checked_mul(4))
        .ok_or(ValueContractError::SizeOverflow)?;
    complete
        .checked_add([0, 2, 3][decoded % 3])
        .ok_or(ValueContractError::SizeOverflow)
}

fn jcs_string_size(value: &str) -> Result<usize, ValueContractError> {
    let mut size = 2_usize;
    for character in value.chars() {
        let additional = match character {
            '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            other => other.len_utf8(),
        };
        checked_add(&mut size, additional)?;
    }
    Ok(size)
}

fn checked_add(size: &mut usize, additional: usize) -> Result<(), ValueContractError> {
    *size = size
        .checked_add(additional)
        .ok_or(ValueContractError::SizeOverflow)?;
    Ok(())
}

fn unsigned_decimal_size(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn signed_decimal_size(value: i64) -> usize {
    unsigned_decimal_size(value.unsigned_abs()) + usize::from(value.is_negative())
}

fn push_fields<'value, 'contract>(
    values: &'value BTreeMap<String, TypedValue>,
    fields: &'contract BTreeMap<String, ValueContractField>,
    pending: &mut Vec<(&'value TypedValue, &'contract TypeRef)>,
) -> Result<(), ValueContractError> {
    for name in values.keys() {
        if !fields.contains_key(name) {
            return Err(ValueContractError::InvalidValue(alloc::format!(
                "unknown field `{name}`"
            )));
        }
    }
    for (name, field) in fields {
        match values.get(name) {
            Some(value) => pending.push((value, &field.type_ref)),
            None if field.required => {
                return Err(ValueContractError::InvalidValue(alloc::format!(
                    "required field `{name}` is missing"
                )));
            }
            None => {}
        }
    }
    Ok(())
}

fn validate_scalar(value: &TypedValue, scalar: &ValueScalar) -> Result<(), ValueContractError> {
    let valid = match scalar {
        ValueScalar::Boolean => matches!(value, TypedValue::Boolean(_)),
        ValueScalar::String => matches!(value, TypedValue::String(_)),
        ValueScalar::Int32 => {
            matches!(
                value,
                TypedValue::Number(CanonicalNumber::I64(number))
                    if (i32::MIN as i64..=i32::MAX as i64).contains(number)
            ) || matches!(
                value,
                TypedValue::Number(CanonicalNumber::U64(number)) if *number <= i32::MAX as u64
            )
        }
        ValueScalar::Int64 => {
            matches!(value, TypedValue::Number(CanonicalNumber::I64(_)))
                || matches!(
                    value,
                    TypedValue::Number(CanonicalNumber::U64(number)) if *number <= i64::MAX as u64
                )
        }
        ValueScalar::UInt64 => {
            matches!(value, TypedValue::Number(CanonicalNumber::U64(_)))
                || matches!(
                    value,
                    TypedValue::Number(CanonicalNumber::I64(number)) if *number >= 0
                )
        }
        ValueScalar::Decimal => matches!(
            value,
            TypedValue::Number(
                CanonicalNumber::I64(_) | CanonicalNumber::U64(_) | CanonicalNumber::Decimal(_)
            )
        ),
        ValueScalar::Uuid => {
            matches!(value, TypedValue::String(value) if valid_canonical_uuid(value))
        }
        ValueScalar::Date => matches!(value, TypedValue::String(value) if valid_date(value)),
        ValueScalar::Timestamp => {
            matches!(value, TypedValue::String(value) if valid_timestamp(value, false))
        }
        ValueScalar::TimestampTz => {
            matches!(value, TypedValue::String(value) if valid_timestamp(value, true))
        }
        ValueScalar::Json => valid_json_shape(value),
        ValueScalar::Custom { .. } => matches!(
            value,
            TypedValue::Boolean(_)
                | TypedValue::String(_)
                | TypedValue::Number(
                    CanonicalNumber::I64(_) | CanonicalNumber::U64(_) | CanonicalNumber::Decimal(_)
                )
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(ValueContractError::InvalidValue(String::from(
            "value does not match its declared scalar",
        )))
    }
}

fn valid_json_shape(value: &TypedValue) -> bool {
    let mut pending = alloc::vec![value];
    while let Some(value) = pending.pop() {
        match value {
            TypedValue::Null | TypedValue::Boolean(_) | TypedValue::String(_) => {}
            TypedValue::Number(CanonicalNumber::I64(_) | CanonicalNumber::U64(_)) => {}
            TypedValue::Number(CanonicalNumber::Decimal(_)) => {}
            TypedValue::List(values) => pending.extend(values),
            TypedValue::Object(values) => pending.extend(values.values()),
            TypedValue::InlineBytes(_) => return false,
        }
    }
    true
}

fn fields_assignable<'a>(
    target: &'a BTreeMap<String, ValueContractField>,
    source: &'a BTreeMap<String, ValueContractField>,
    pending: &mut Vec<(&'a TypeRef, &'a TypeRef)>,
) -> bool {
    if target.len() != source.len() {
        return false;
    }
    for (name, target_field) in target {
        let Some(source_field) = source.get(name) else {
            return false;
        };
        if target_field.required != source_field.required {
            return false;
        }
        pending.push((&target_field.type_ref, &source_field.type_ref));
    }
    true
}

fn valid_canonical_decimal(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut offset = 0;
    if bytes.get(offset) == Some(&b'-') {
        offset += 1;
    }
    match bytes.get(offset) {
        Some(b'0') => {
            offset += 1;
            if bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                return false;
            }
        }
        Some(b'1'..=b'9') => {
            offset += 1;
            while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                offset += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == start || bytes.get(offset.wrapping_sub(1)) == Some(&b'0') {
            return false;
        }
    }
    offset == bytes.len() && value != "-0"
}

fn valid_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    valid_date_bytes(bytes)
}

fn valid_date_bytes(bytes: &[u8]) -> bool {
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = decimal_digits(&bytes[0..4]);
    let month = decimal_digits(&bytes[5..7]);
    let day = decimal_digits(&bytes[8..10]);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=maximum_day).contains(&day)
}

fn valid_timestamp(value: &str, with_time_zone: bool) -> bool {
    let bytes = value.as_bytes();
    let local_end = if with_time_zone {
        if bytes.last() == Some(&b'Z') {
            bytes.len() - 1
        } else if bytes.len() >= 6 {
            let offset = &bytes[bytes.len() - 6..];
            if !matches!(offset.first(), Some(b'+' | b'-'))
                || offset.get(3) != Some(&b':')
                || !offset[1..3].iter().all(u8::is_ascii_digit)
                || !offset[4..6].iter().all(u8::is_ascii_digit)
                || decimal_digits(&offset[1..3]) > 23
                || decimal_digits(&offset[4..6]) > 59
            {
                return false;
            }
            bytes.len() - 6
        } else {
            return false;
        }
    } else {
        bytes.len()
    };
    let local = &bytes[..local_end];
    if local.len() < 19 || local.get(10) != Some(&b'T') {
        return false;
    }
    if !valid_date_bytes(&local[..10]) {
        return false;
    }
    let time = &local[11..];
    let (clock, fraction) = match time.iter().position(|byte| *byte == b'.') {
        Some(index) => (&time[..index], Some(&time[index + 1..])),
        None => (time, None),
    };
    if clock.len() != 8
        || clock[2] != b':'
        || clock[5] != b':'
        || !clock
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 5) || byte.is_ascii_digit())
        || decimal_digits(&clock[0..2]) > 23
        || decimal_digits(&clock[3..5]) > 59
        || decimal_digits(&clock[6..8]) > 59
    {
        return false;
    }
    fraction.is_none_or(|fraction| {
        (1..=6).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit)
    })
}

fn decimal_digits(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
}
