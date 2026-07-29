use crate::AbiError;

pub const ABI_ID_CAPACITY: usize = 96;

#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct InlineId {
    len: u8,
    bytes: [u8; ABI_ID_CAPACITY],
}

impl InlineId {
    /// Builds an ID for checked-in descriptors and static processor tables.
    ///
    /// Invalid literals fail during const evaluation:
    ///
    /// ```compile_fail
    /// use donat_connector_abi::InlineId;
    ///
    /// const INVALID: InlineId = InlineId::literal("trailing-");
    /// ```
    pub const fn literal(value: &'static str) -> Self {
        let bytes = value.as_bytes();
        assert!(valid_id(bytes), "invalid connector ABI ID literal");
        Self::copy_bytes(bytes)
    }

    pub fn parse(value: &str) -> Result<Self, AbiError> {
        let bytes = value.as_bytes();
        if valid_id(bytes) {
            Ok(Self::copy_bytes(bytes))
        } else {
            Err(AbiError::InvalidId)
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("validated connector ABI IDs are ASCII")
    }

    const fn copy_bytes(source: &[u8]) -> Self {
        let mut bytes = [0_u8; ABI_ID_CAPACITY];
        let mut index = 0;
        while index < source.len() {
            bytes[index] = source[index];
            index += 1;
        }
        Self {
            len: source.len() as u8,
            bytes,
        }
    }
}

const fn valid_id(value: &[u8]) -> bool {
    if value.is_empty() || value.len() > ABI_ID_CAPACITY {
        return false;
    }

    let mut index = 0;
    while index < value.len() {
        let byte = value[index];
        let is_alphanumeric = byte.is_ascii_lowercase() || byte.is_ascii_digit();
        let is_separator = matches!(byte, b'.' | b'-' | b'_');
        if !is_alphanumeric && !is_separator {
            return false;
        }
        if (index == 0 || index + 1 == value.len()) && !is_alphanumeric {
            return false;
        }
        index += 1;
    }
    true
}

macro_rules! define_id {
    ($name:ident) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
        pub struct $name(InlineId);

        impl $name {
            pub const fn literal(value: &'static str) -> Self {
                Self(InlineId::literal(value))
            }

            pub fn parse(value: &str) -> Result<Self, AbiError> {
                InlineId::parse(value).map(Self)
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}

define_id!(ConnectorId);
define_id!(OperationId);
define_id!(CompiledStepId);
define_id!(ProcessorFamilyId);
define_id!(AuthenticatorId);
define_id!(CodecId);
define_id!(NormalizerId);
define_id!(TriggerId);
define_id!(CredentialSpecId);
define_id!(CredentialFieldId);
define_id!(CapabilityId);
define_id!(BindingSlotId);
define_id!(OriginId);
