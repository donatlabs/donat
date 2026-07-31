use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::ops::Range;

use base64::Engine;
use donat_connector_abi::{
    AuthenticatorId, CapabilityId, CodecId, CompiledStepId, ConnectorErrorClass, ConnectorId,
    CredentialFieldId, CredentialSpecId, Hash256 as AbiHash256, NormalizerId, OperationId,
    OriginId, ProcessorFamilyId, TriggerId,
};
use donat_value_contract::{
    CanonicalNumber, TypeRef, TypedValue, ValueContractCatalog, ValueContractField,
    ValueObjectContract, ValueScalar, ValueType,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::{
    AcceptedRecordCatalog, ArtifactHash, CatalogError, ConnectorSourceRecord, ContractFact,
    DonatPolicyId, ExactFactLocation, ProviderContractReference, ResolvedContractFactBinding,
    ResolvedFactValue, SelectedResponseHeader, SourceRecordId, SourceSubject, StableSemver,
};

const SOURCE_RECORD_DOMAIN: &[u8] = b"donat.connector.source-record.v1\0";
const SEMANTIC_DOMAIN: &[u8] = b"donat.connector.semantic.v1\0";
const PROVENANCE_DOMAIN: &[u8] = b"donat.connector.provenance.v1\0";
const VALUE_CONTRACT_DOMAIN: &[u8] = b"donat.connector.value-contract.v1\0";
const RESPONSE_HEADER_DOMAIN: &[u8] = b"donat.connector.response-header-capability.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogHashDomain {
    SourceRecord,
    Semantic,
    Provenance,
    ValueContract,
}

impl CatalogHashDomain {
    const fn prefix(self) -> &'static [u8] {
        match self {
            Self::SourceRecord => SOURCE_RECORD_DOMAIN,
            Self::Semantic => SEMANTIC_DOMAIN,
            Self::Provenance => PROVENANCE_DOMAIN,
            Self::ValueContract => VALUE_CONTRACT_DOMAIN,
        }
    }
}

fn domain_hash_bytes(domain: CatalogHashDomain, canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain.prefix());
    hash.update(canonical_bytes);
    hash.finalize().into()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CanonicalOwnerPathDescriptor {
    pub normalized_owner: &'static str,
    pub normalized_member: &'static str,
    pub normalized_source: CanonicalDeclarationSource,
    pub domain: &'static str,
    pub canonical_path: &'static str,
    pub owner_class: &'static str,
    pub order: &'static str,
    pub null_empty: &'static str,
    pub branch_type: &'static str,
    pub material_member: &'static str,
    pub material_source: CanonicalDeclarationSource,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalDeclarationSource {
    Source,
    Model,
    ValueContract,
    ConnectorAbi,
    ProjectionSchema,
    BuilderDerived,
    Constant,
    NamedDerived,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CanonicalMutationDescriptor {
    pub case: CanonicalMutationCase,
    pub disposition: CanonicalMutationDisposition,
    pub material_source: CanonicalDeclarationSource,
    pub domain: &'static str,
    pub canonical_path: &'static str,
    pub material_member: &'static str,
    pub branch_type: &'static str,
    pub null_empty: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CanonicalDerivedDependencyDescriptor {
    pub changed_input: &'static str,
    pub material_member: &'static str,
    pub rule: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalMutationDisposition {
    Mutable,
    Singleton,
    PublicPipelineRejected,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CanonicalMutationCase {
    SourceRecord,
    Semantic,
    Provenance,
    ValueContract,
    TypedValue,
}

macro_rules! declare_canonical_projection_route_model {
    (
        route_id $route_id:ident;
        producer $producer:ident;
        input_binding $input_binding:ident;
        assignment $assignment:ident;
        public_probe $public_probe:ident;
        probe_disposition $probe_disposition:ident;
        probe_membership $probe_membership:ident;
        mount $mount:ident;
        mount_segment $mount_segment:ident;
        key_part $key_part:ident;
        static_segment $static_segment:ident;
        dependency_edge $dependency_edge:ident;
        route $route:ident;
    ) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $route_id {
            pub case: CanonicalMutationCase,
            pub material_owner: &'static str,
            pub material_field: &'static str,
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $producer {
            PublicBuilder { function: &'static str },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $input_binding {
            PublicParameter {
                parameter: &'static str,
                validated_context_owner: &'static str,
                validated_context_field: &'static str,
            },
            NormalizedMember {
                normalized_owner: &'static str,
                normalized_member: &'static str,
            },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $assignment {
            ValidatedContext {
                source_context_owner: &'static str,
                source_context_field: &'static str,
                target: $route_id,
            },
            NormalizedMember {
                normalized_owner: &'static str,
                normalized_member: &'static str,
                target: $route_id,
            },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $public_probe {
            pub case: CanonicalMutationCase,
            pub owner: &'static str,
            pub group: &'static str,
        }

        impl $public_probe {
            pub const fn new(
                case: CanonicalMutationCase,
                owner: &'static str,
                group: &'static str,
            ) -> Self {
                Self { case, owner, group }
            }
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $probe_disposition {
            Accepted,
            PublicPipelineRejected,
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $probe_membership {
            pub probe: $public_probe,
            pub disposition: $probe_disposition,
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $mount {
            RootField { canonical_json_path: &'static str },
            SourcePath { segments: &'static [$mount_segment] },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $mount_segment {
            Field(&'static str),
            TaggedKind { expected_kind: &'static str },
            TaggedValue { expected_kind: &'static str },
            KeyedElement { key: &'static [$key_part] },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $key_part {
            pub path: &'static [$static_segment],
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $static_segment {
            Field(&'static str),
            TaggedValue { expected_kind: &'static str },
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $dependency_edge {
            pub dependent_route: $route_id,
        }

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $route {
            pub route_id: $route_id,
            pub owner: CanonicalOwnerPathDescriptor,
            pub producer: $producer,
            pub input_binding: $input_binding,
            pub assignment: $assignment,
            pub disposition: CanonicalMutationDisposition,
            pub probe_memberships: &'static [$probe_membership],
            pub mounts: &'static [$mount],
            pub dependency_edges: &'static [$dependency_edge],
        }
    };
}

declare_canonical_projection_route_model! {
    route_id CanonicalProjectionRouteId;
    producer CanonicalProjectionProducer;
    input_binding CanonicalProjectionInputBinding;
    assignment CanonicalProjectionAssignment;
    public_probe CanonicalPublicInputProbeId;
    probe_disposition CanonicalProjectionProbeDisposition;
    probe_membership CanonicalProjectionProbeMembership;
    mount CanonicalProjectionMount;
    mount_segment CanonicalProjectionMountSegment;
    key_part CanonicalProjectionKeyPart;
    static_segment CanonicalProjectionStaticSegment;
    dependency_edge CanonicalProjectionDependencyEdge;
    route CanonicalProjectionRoute;
}

macro_rules! project_value_contract_value {
    ($value:expr, copy) => {
        Ok::<_, CatalogError>(*$value)
    };
    ($value:expr, clone) => {
        Ok::<_, CatalogError>($value.clone())
    };
    ($value:expr, project($projector:ident)) => {
        $projector($value)
    };
    ($value:expr, boxed_project($projector:ident)) => {
        $projector($value.as_ref()).map(Box::new)
    };
    ($value:expr, validated_clone) => {{
        validate_material_name($value)?;
        Ok::<_, CatalogError>($value.clone())
    }};
    ($value:expr, validated_list_clone) => {{
        for member in $value {
            validate_material_name(member)?;
        }
        Ok::<_, CatalogError>($value.clone())
    }};
    ($value:expr, map($projector:ident)) => {
        $value
            .iter()
            .map(|(name, value)| $projector(value).map(|material| (name.clone(), material)))
            .collect::<Result<_, CatalogError>>()
    };
    ($value:expr, validated_map($projector:ident)) => {
        $value
            .iter()
            .map(|(name, value)| {
                validate_material_name(name)?;
                $projector(value).map(|material| (name.clone(), material))
            })
            .collect::<Result<_, CatalogError>>()
    };
}

macro_rules! validate_value_contract_parameter {
    ($value:expr, nonzero_epoch) => {{
        if $value == 0 {
            Err(CatalogError::new(
                "catalog_projection_input_mismatch",
                "value-language epoch must be nonzero",
            ))
        } else {
            Ok($value)
        }
    }};
}

macro_rules! project_typed_value {
    ($value:expr, copy) => {
        *$value
    };
    ($value:expr, clone) => {
        $value.clone()
    };
    ($value:expr, integer_string) => {
        $value.to_string()
    };
    ($value:expr, decimal_string) => {
        $value.as_str().to_owned()
    };
    ($value:expr, recursive_list($projector:ident)) => {
        $value.iter().map($projector).collect()
    };
    ($value:expr, recursive_map($projector:ident)) => {
        $value
            .iter()
            .map(|(name, member)| (name.clone(), $projector(member)))
            .collect()
    };
    ($value:expr, base64url) => {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode($value.as_slice())
    };
    ($value:expr, file_name) => {
        $value.file_name().map(str::to_owned)
    };
    ($value:expr, media_type) => {
        Some($value.media_type().to_owned())
    };
}

macro_rules! project_source_value {
    ($value:expr, copy) => {
        Ok::<_, CatalogError>(*$value)
    };
    ($value:expr, clone) => {
        Ok::<_, CatalogError>((*$value).clone())
    };
    ($value:expr, to_string) => {
        Ok::<_, CatalogError>($value.to_string())
    };
    ($value:expr, as_str_owned) => {
        Ok::<_, CatalogError>($value.as_str().to_owned())
    };
    ($value:expr, optional_to_string) => {
        Ok::<_, CatalogError>($value.as_ref().map(ToString::to_string))
    };
    ($value:expr, strings) => {
        Ok::<_, CatalogError>($value.iter().map(ToString::to_string).collect())
    };
    ($value:expr, sorted_unique_strings) => {
        sorted_unique_strings($value)
    };
    ($value:expr, sorted_unique_ids) => {{
        let mut values = $value
            .iter()
            .map(|value| value.as_str().to_owned())
            .collect::<Vec<_>>();
        values.sort();
        reject_adjacent_duplicate(values.iter().map(String::as_str))?;
        Ok::<_, CatalogError>(values)
    }};
    ($value:expr, project($projector:ident)) => {
        $projector($value)
    };
    ($value:expr, npm_algorithm($projector:ident)) => {
        $projector(&$value.algorithm())
    };
    ($value:expr, map_project($projector:ident)) => {
        $value
            .iter()
            .map($projector)
            .collect::<Result<Vec<_>, CatalogError>>()
    };
    (
        $value:expr,
        sorted_project($projector:ident, $key:ident)
    ) => {{
        let mut projected = $value
            .iter()
            .map($projector)
            .collect::<Result<Vec<_>, CatalogError>>()?;
        projected.sort_by(|left, right| $key(left).cmp(&$key(right)));
        Ok::<_, CatalogError>(projected)
    }};
    (
        $value:expr,
        unique_sorted_project($projector:ident, $key:ident)
    ) => {{
        let mut projected = $value
            .iter()
            .map($projector)
            .collect::<Result<Vec<_>, CatalogError>>()?;
        projected.sort_by(|left, right| $key(left).cmp(&$key(right)));
        reject_adjacent_duplicate(projected.iter().map($key))?;
        Ok::<_, CatalogError>(projected)
    }};
    ($value:expr, base64url) => {
        Ok::<_, CatalogError>(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode($value.as_bytes()),
        )
    };
}

macro_rules! project_source_field {
    (
        $value:ident,
        $field:ident,
        member { $($projection:tt)+ }
    ) => {
        project_source_value!(&$value.$field, $($projection)+)
    };
    (
        $value:ident,
        $field:ident,
        whole { $($projection:tt)+ }
    ) => {
        project_source_value!($value, $($projection)+)
    };
}

macro_rules! source_route_disposition {
    () => {
        CanonicalMutationDisposition::Mutable
    };
    (Singleton) => {
        CanonicalMutationDisposition::Singleton
    };
    (LoaderRejected) => {
        CanonicalMutationDisposition::PublicPipelineRejected
    };
    (ExecutableRejected) => {
        CanonicalMutationDisposition::Mutable
    };
}

macro_rules! source_route_probe_memberships {
    ($owner:expr, $field:expr) => {
        &[CanonicalProjectionProbeMembership {
            probe: CanonicalPublicInputProbeId::new(
                CanonicalMutationCase::SourceRecord,
                $owner,
                $field,
            ),
            disposition: CanonicalProjectionProbeDisposition::Accepted,
        }]
    };
    ($owner:expr, $field:expr, Singleton) => {
        &[]
    };
    ($owner:expr, $field:expr, LoaderRejected) => {
        &[CanonicalProjectionProbeMembership {
            probe: CanonicalPublicInputProbeId::new(
                CanonicalMutationCase::SourceRecord,
                $owner,
                $field,
            ),
            disposition: CanonicalProjectionProbeDisposition::PublicPipelineRejected,
        }]
    };
    ($owner:expr, $field:expr, ExecutableRejected) => {
        &[
            CanonicalProjectionProbeMembership {
                probe: CanonicalPublicInputProbeId::new(
                    CanonicalMutationCase::SourceRecord,
                    $owner,
                    $field,
                ),
                disposition: CanonicalProjectionProbeDisposition::Accepted,
            },
            CanonicalProjectionProbeMembership {
                probe: CanonicalPublicInputProbeId::new(
                    CanonicalMutationCase::SourceRecord,
                    $owner,
                    "ExecutablePublicPipeline",
                ),
                disposition: CanonicalProjectionProbeDisposition::PublicPipelineRejected,
            },
        ]
    };
}

macro_rules! source_mount_field {
    ($field:literal) => {
        CanonicalProjectionMountSegment::Field($field)
    };
}

macro_rules! source_mount_tagged {
    ($kind:literal) => {
        CanonicalProjectionMountSegment::TaggedValue {
            expected_kind: $kind,
        }
    };
}

macro_rules! source_mount_static_segment {
    (field $field:literal) => {
        CanonicalProjectionStaticSegment::Field($field)
    };
    (tagged $kind:literal) => {
        CanonicalProjectionStaticSegment::TaggedValue {
            expected_kind: $kind,
        }
    };
}

macro_rules! source_mount_key {
    (
        $(
            [
                $(
                    $segment_kind:ident $segment_value:literal
                ),+ $(,)?
            ]
        ),+ $(,)?
    ) => {
        CanonicalProjectionMountSegment::KeyedElement {
            key: &[
                $(
                    CanonicalProjectionKeyPart {
                        path: &[
                            $(
                                source_mount_static_segment!(
                                    $segment_kind $segment_value
                                ),
                            )+
                        ],
                    },
                )+
            ],
        }
    };
}

macro_rules! source_loader_branch {
    ($member:expr $(,)?) => {
        Some($member)
    };
    ($member:expr, LoaderRejected $(,)?) => {
        None
    };
    ($member:expr, ExecutableRejected $(,)?) => {
        Some($member)
    };
}

macro_rules! source_field_route {
    (
        $builder:ident,
        $material:ident,
        mounts {
            $(
                [$($mount_segment:expr),* $(,)?];
            )+
        },
        $owner:expr,
        $path:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr,
        $field:ident
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id: CanonicalProjectionRouteId {
                case: CanonicalMutationCase::SourceRecord,
                material_owner: stringify!($material),
                material_field: stringify!($field),
            },
            owner: CanonicalOwnerPathDescriptor {
                normalized_owner: $owner,
                normalized_member: $owner,
                normalized_source: CanonicalDeclarationSource::Source,
                domain: "source-record",
                canonical_path: $path,
                owner_class: "normalized",
                order: $order,
                null_empty: $null_empty,
                branch_type: $branch,
                material_member: $material_member,
                material_source: CanonicalDeclarationSource::ProjectionSchema,
            },
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
                target: CanonicalProjectionRouteId {
                    case: CanonicalMutationCase::SourceRecord,
                    material_owner: stringify!($material),
                    material_field: stringify!($field),
                },
            },
            disposition: source_route_disposition!($($disposition)?),
            probe_memberships: source_route_probe_memberships!(
                stringify!($material),
                stringify!($field)
                $(, $disposition)?
            ),
            mounts: &[
                $(
                    CanonicalProjectionMount::SourcePath {
                        segments: &[
                            $($mount_segment,)*
                            CanonicalProjectionMountSegment::Field(
                                stringify!($field),
                            ),
                        ],
                    },
                )+
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! source_variant_route {
    (
        $builder:ident,
        $material:ident,
        mounts {
            $(
                [$($mount_segment:expr),* $(,)?];
            )+
        },
        $path:expr,
        $owner:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr,
        $variant:ident,
        $wire:expr
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id: CanonicalProjectionRouteId {
                case: CanonicalMutationCase::SourceRecord,
                material_owner: concat!(stringify!($material), "::", stringify!($variant)),
                material_field: "kind",
            },
            owner: CanonicalOwnerPathDescriptor {
                normalized_owner: $owner,
                normalized_member: $owner,
                normalized_source: CanonicalDeclarationSource::Source,
                domain: "source-record",
                canonical_path: $path,
                owner_class: "normalized",
                order: $order,
                null_empty: $null_empty,
                branch_type: $branch,
                material_member: $material_member,
                material_source: CanonicalDeclarationSource::ProjectionSchema,
            },
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
                target: CanonicalProjectionRouteId {
                    case: CanonicalMutationCase::SourceRecord,
                    material_owner: concat!(stringify!($material), "::", stringify!($variant)),
                    material_field: "kind",
                },
            },
            disposition: source_route_disposition!($($disposition)?),
            probe_memberships: source_route_probe_memberships!(
                concat!(stringify!($material), "::", stringify!($variant)),
                "kind"
                $(, $disposition)?
            ),
            mounts: &[
                $(
                    CanonicalProjectionMount::SourcePath {
                        segments: &[
                            $($mount_segment,)*
                            CanonicalProjectionMountSegment::TaggedKind {
                                expected_kind: $wire,
                            },
                        ],
                    },
                )+
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! source_variant_field_route {
    (
        $builder:ident,
        $material:ident,
        mounts {
            $(
                [$($mount_segment:expr),* $(,)?];
            )+
        },
        $variant:ident,
        $wire:expr,
        $owner:expr,
        $path:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr,
        $field:ident
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id: CanonicalProjectionRouteId {
                case: CanonicalMutationCase::SourceRecord,
                material_owner: concat!(stringify!($material), "::", stringify!($variant)),
                material_field: stringify!($field),
            },
            owner: CanonicalOwnerPathDescriptor {
                normalized_owner: $owner,
                normalized_member: $owner,
                normalized_source: CanonicalDeclarationSource::Source,
                domain: "source-record",
                canonical_path: $path,
                owner_class: "normalized",
                order: $order,
                null_empty: $null_empty,
                branch_type: $branch,
                material_member: $material_member,
                material_source: CanonicalDeclarationSource::ProjectionSchema,
            },
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
                target: CanonicalProjectionRouteId {
                    case: CanonicalMutationCase::SourceRecord,
                    material_owner: concat!(stringify!($material), "::", stringify!($variant)),
                    material_field: stringify!($field),
                },
            },
            disposition: source_route_disposition!($($disposition)?),
            probe_memberships: source_route_probe_memberships!(
                concat!(stringify!($material), "::", stringify!($variant)),
                stringify!($field)
                $(, $disposition)?
            ),
            mounts: &[
                $(
                    CanonicalProjectionMount::SourcePath {
                        segments: &[
                            $($mount_segment,)*
                            CanonicalProjectionMountSegment::TaggedValue {
                                expected_kind: $wire,
                            },
                            CanonicalProjectionMountSegment::Field(
                                stringify!($field),
                            ),
                        ],
                    },
                )+
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! source_singleton_route {
    (
        $builder:ident,
        $material:ident,
        mounts {
            $(
                [$($mount_segment:expr),* $(,)?];
            )+
        },
        $field:ident,
        $variant:ident,
        $wire:expr,
        $owner:expr,
        $path:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id: CanonicalProjectionRouteId {
                case: CanonicalMutationCase::SourceRecord,
                material_owner: stringify!($material),
                material_field: stringify!($variant),
            },
            owner: CanonicalOwnerPathDescriptor {
                normalized_owner: $owner,
                normalized_member: $owner,
                normalized_source: CanonicalDeclarationSource::Source,
                domain: "source-record",
                canonical_path: $path,
                owner_class: "normalized",
                order: $order,
                null_empty: $null_empty,
                branch_type: $branch,
                material_member: $material_member,
                material_source: CanonicalDeclarationSource::ProjectionSchema,
            },
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $owner,
                target: CanonicalProjectionRouteId {
                    case: CanonicalMutationCase::SourceRecord,
                    material_owner: stringify!($material),
                    material_field: stringify!($variant),
                },
            },
            disposition: source_route_disposition!($($disposition)?),
            probe_memberships: source_route_probe_memberships!(
                stringify!($material),
                stringify!($variant)
                $(, $disposition)?
            ),
            mounts: &[
                $(
                    CanonicalProjectionMount::SourcePath {
                        segments: &[
                            $($mount_segment,)*
                            CanonicalProjectionMountSegment::Field(
                                stringify!($field),
                            ),
                            CanonicalProjectionMountSegment::TaggedKind {
                                expected_kind: $wire,
                            },
                        ],
                    },
                )+
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! source_singleton_route_from_variants {
    (
        $builder:ident,
        $material:ident,
        mounts {
            $(
                [$($mount_segment:expr),* $(,)?];
            )+
        },
        $field:ident,
        $owner:expr,
        $path:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr,
        variants [$variant:ident as $wire:literal $(,)?]
        $(, $disposition:ident)? $(,)?
    ) => {
        source_singleton_route!(
            $builder,
            $material,
            mounts {
                $(
                    [$($mount_segment),*];
                )+
            },
            $field,
            $variant,
            $wire,
            $owner,
            $path,
            $order,
            $null_empty,
            $branch,
            $material_member
            $(, $disposition)?,
        )
    };
}

const fn source_mutation_view(route: CanonicalProjectionRoute) -> CanonicalMutationDescriptor {
    CanonicalMutationDescriptor {
        case: CanonicalMutationCase::SourceRecord,
        disposition: route.disposition,
        material_source: route.owner.material_source,
        domain: route.owner.domain,
        canonical_path: route.owner.canonical_path,
        material_member: route.owner.material_member,
        branch_type: route.owner.branch_type,
        null_empty: route.owner.null_empty,
    }
}

macro_rules! semantic_owner_descriptor {
    (
        $owner:expr,
        $normalized_member:expr,
        $normalized_source:ident,
        $path:expr,
        $class:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr $(,)?
    ) => {
        CanonicalOwnerPathDescriptor {
            normalized_owner: $owner,
            normalized_member: $normalized_member,
            normalized_source: CanonicalDeclarationSource::$normalized_source,
            domain: "semantic",
            canonical_path: $path,
            owner_class: $class,
            order: $order,
            null_empty: $null_empty,
            branch_type: $branch,
            material_member: $material_member,
            material_source: CanonicalDeclarationSource::ProjectionSchema,
        }
    };
}

macro_rules! semantic_mutation_descriptor {
    (
        $path:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalMutationDescriptor {
            case: CanonicalMutationCase::Semantic,
            disposition:
                semantic_mutation_disposition!($($disposition)?),
            material_source: CanonicalDeclarationSource::ProjectionSchema,
            domain: "semantic",
            canonical_path: $path,
            material_member: $material_member,
            branch_type: $branch,
            null_empty: $null_empty,
        }
    };
}

macro_rules! semantic_mutation_disposition {
    () => {
        CanonicalMutationDisposition::Mutable
    };
    (Singleton) => {
        CanonicalMutationDisposition::Singleton
    };
}

macro_rules! value_contract_owner_descriptor {
    (
        $owner:expr,
        $normalized_member:expr,
        $normalized_source:ident,
        $path:expr,
        $class:expr,
        $order:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr $(,)?
    ) => {
        CanonicalOwnerPathDescriptor {
            normalized_owner: $owner,
            normalized_member: $normalized_member,
            normalized_source: CanonicalDeclarationSource::$normalized_source,
            domain: "value-contract",
            canonical_path: $path,
            owner_class: $class,
            order: $order,
            null_empty: $null_empty,
            branch_type: $branch,
            material_member: $material_member,
            material_source: CanonicalDeclarationSource::ProjectionSchema,
        }
    };
}

macro_rules! value_contract_mutation_descriptor {
    (
        $path:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr
        $(, $disposition:ident)? $(,)?
    ) => {
        CanonicalMutationDescriptor {
            case: CanonicalMutationCase::ValueContract,
            disposition:
                value_contract_mutation_disposition!($($disposition)?),
            material_source: CanonicalDeclarationSource::ProjectionSchema,
            domain: "value-contract",
            canonical_path: $path,
            material_member: $material_member,
            branch_type: $branch,
            null_empty: $null_empty,
        }
    };
}

macro_rules! value_contract_mutation_disposition {
    () => {
        CanonicalMutationDisposition::Mutable
    };
    ($disposition:ident) => {
        CanonicalMutationDisposition::$disposition
    };
}

macro_rules! value_contract_route_id {
    ($material_owner:ident, $material_field:ident) => {
        CanonicalProjectionRouteId {
            case: CanonicalMutationCase::ValueContract,
            material_owner: stringify!($material_owner),
            material_field: stringify!($material_field),
        }
    };
}

macro_rules! value_contract_input_binding {
    (
        PublicParameter,
        $context_owner:ident,
        $context_field:ident $(,)?
    ) => {
        CanonicalProjectionInputBinding::PublicParameter {
            parameter: stringify!($context_field),
            validated_context_owner: stringify!($context_owner),
            validated_context_field: stringify!($context_field),
        }
    };
}

macro_rules! value_contract_route_mount {
    (RootField, $material_field:ident) => {
        CanonicalProjectionMount::RootField {
            canonical_json_path: concat!("$.", stringify!($material_field)),
        }
    };
}

macro_rules! typed_value_mutation_descriptor {
    (
        $path:expr,
        $null_empty:expr,
        $branch:expr,
        $material_member:expr $(,)?
    ) => {
        CanonicalMutationDescriptor {
            case: CanonicalMutationCase::TypedValue,
            disposition: CanonicalMutationDisposition::Mutable,
            material_source: CanonicalDeclarationSource::ProjectionSchema,
            domain: "value-contract",
            canonical_path: $path,
            material_member: $material_member,
            branch_type: $branch,
            null_empty: $null_empty,
        }
    };
}

macro_rules! typed_value_kind_route_id {
    ($material:ident, $variant:ident) => {
        CanonicalProjectionRouteId {
            case: CanonicalMutationCase::TypedValue,
            material_owner: concat!(stringify!($material), "::", stringify!($variant),),
            material_field: "kind",
        }
    };
}

macro_rules! typed_value_field_route_id {
    ($material:ident, $variant:ident, $field:ident) => {
        CanonicalProjectionRouteId {
            case: CanonicalMutationCase::TypedValue,
            material_owner: concat!(stringify!($material), "::", stringify!($variant),),
            material_field: stringify!($field),
        }
    };
}

macro_rules! typed_value_probe_id {
    ($input:ident, $group:ident) => {
        CanonicalPublicInputProbeId::new(
            CanonicalMutationCase::TypedValue,
            stringify!($input),
            stringify!($group),
        )
    };
}

macro_rules! typed_value_route_path {
    ($wrapper:ident, $wire:literal, TaggedValue, $field_wire:literal) => {
        concat!(stringify!($wrapper), "{kind=", $wire, "}.value")
    };
    (
        $wrapper:ident,
        $wire:literal,
        TaggedStructField,
        $field_wire:literal
    ) => {
        concat!(
            stringify!($wrapper),
            "{kind=",
            $wire,
            "}.value.",
            $field_wire,
        )
    };
}

macro_rules! typed_value_route_mount {
    (TaggedKind, $field_wire:literal) => {
        CanonicalProjectionMount::RootField {
            canonical_json_path: "$.kind",
        }
    };
    (TaggedValue, $field_wire:literal) => {
        CanonicalProjectionMount::RootField {
            canonical_json_path: "$.value",
        }
    };
    (TaggedStructField, $field_wire:literal) => {
        CanonicalProjectionMount::RootField {
            canonical_json_path: concat!("$.value.", $field_wire),
        }
    };
}

macro_rules! typed_value_kind_route {
    (
        $wrapper:ident,
        $material:ident,
        $input:ident,
        $public_builder:ident,
        $owner:literal,
        $normalized:literal,
        $variant:ident,
        $wire:literal,
        $mount:ident,
        probes [$($probe:ident),+ $(,)?] $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id: typed_value_kind_route_id!($material, $variant),
            owner: value_contract_owner_descriptor!(
                $owner,
                $normalized,
                ValueContract,
                concat!(stringify!($wrapper), "{kind=", $wire, "}.kind"),
                "normalized",
                "scalar",
                "required",
                $wire,
                concat!(stringify!($material), "::", stringify!($variant)),
            ),
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($public_builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $normalized,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $normalized,
                target: typed_value_kind_route_id!($material, $variant),
            },
            disposition: CanonicalMutationDisposition::Mutable,
            probe_memberships: &[
                $(
                    CanonicalProjectionProbeMembership {
                        probe: typed_value_probe_id!($input, $probe),
                        disposition:
                            CanonicalProjectionProbeDisposition::Accepted,
                    },
                )+
            ],
            mounts: &[
                typed_value_route_mount!($mount, "kind"),
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! typed_value_field_route {
    (
        $wrapper:ident,
        $material:ident,
        $input:ident,
        $public_builder:ident,
        $owner:literal,
        $normalized:literal,
        $variant:ident,
        $wire:literal,
        $field:ident,
        $field_wire:literal,
        $order:literal,
        $null_empty:literal,
        $branch:literal,
        $mount:ident,
        probes [$($probe:ident),+ $(,)?] $(,)?
    ) => {
        CanonicalProjectionRoute {
            route_id:
                typed_value_field_route_id!($material, $variant, $field),
            owner: value_contract_owner_descriptor!(
                $owner,
                $normalized,
                ValueContract,
                typed_value_route_path!(
                    $wrapper,
                    $wire,
                    $mount,
                    $field_wire
                ),
                "normalized",
                $order,
                $null_empty,
                $branch,
                concat!(
                    stringify!($material),
                    "::",
                    stringify!($variant),
                    ".",
                    stringify!($field),
                ),
            ),
            producer: CanonicalProjectionProducer::PublicBuilder {
                function: stringify!($public_builder),
            },
            input_binding: CanonicalProjectionInputBinding::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $normalized,
            },
            assignment: CanonicalProjectionAssignment::NormalizedMember {
                normalized_owner: $owner,
                normalized_member: $normalized,
                target:
                    typed_value_field_route_id!(
                        $material,
                        $variant,
                        $field
                    ),
            },
            disposition: CanonicalMutationDisposition::Mutable,
            probe_memberships: &[
                $(
                    CanonicalProjectionProbeMembership {
                        probe: typed_value_probe_id!($input, $probe),
                        disposition:
                            CanonicalProjectionProbeDisposition::Accepted,
                    },
                )+
            ],
            mounts: &[
                typed_value_route_mount!($mount, $field_wire),
            ],
            dependency_edges: &[],
        }
    };
}

macro_rules! projection_schema {
    (
        owner_paths {
            $(
                (
                    $owner:literal,
                    $normalized_member:literal,
                    $normalized_source:ident,
                    $domain:literal,
                    $path:literal,
                    $class:literal,
                    $order:literal,
                    $null_empty:literal,
                    $branch:literal,
                    $member:literal,
                    $material_source:ident,
                    $case:ident,
                    $disposition:ident $(,)?
                );
            )*
        }
        value_contract_projection {
            root $value_material:ident via $value_context:ident
                using $value_builder:ident {
                catalog_fields {
                    $(
                        (
                            $value_owner:literal,
                            $value_normalized_member:literal,
                            $value_normalized_source:ident,
                            $value_class:literal,
                            $value_order:literal,
                            $value_null_empty:literal,
                            $value_branch:literal,
                            $value_field:ident : $value_type:ty =
                                $value_projection:ident
                                $(($value_projector:ident))? $(,)?
                        );
                    )*
                }
                derived_fields {
                    $(
                        (
                            $derived_owner:literal,
                            $derived_class:literal,
                            $derived_order:literal,
                            $derived_null_empty:literal,
                            $derived_branch:literal,
                            $derived_field:ident : $derived_type:ty =
                                $derived_validator:ident,
                            route {
                                disposition $derived_disposition:ident;
                                input $derived_input_binding:ident;
                                mount $derived_mount:ident;
                                probes {
                                    $(
                                        $derived_probe:ident =>
                                            $derived_probe_disposition:ident;
                                    )*
                                }
                                dependencies {
                                    $(
                                        $derived_dependency_case:ident =>
                                            $derived_dependency_owner:ident .
                                            $derived_dependency_field:ident;
                                    )*
                                }
                            } $(,)?
                        );
                    )*
                }
            }
            structs {
                $(
                    struct $struct_material:ident from $struct_input:ident
                        using $struct_projector:ident {
                        $(
                            (
                                $struct_owner:literal,
                                $struct_normalized_member:literal,
                                $struct_class:literal,
                                $struct_order:literal,
                                $struct_null_empty:literal,
                                $struct_branch:literal,
                                $struct_field:ident : $struct_type:ty =
                                    $struct_projection:ident
                                    $(($struct_member_projector:ident))? $(,)?
                            );
                        )*
                    }
                )*
            }
            transparent_enums {
                $(
                    transparent $enum_wrapper:ident wraps $enum_material:ident
                        from $enum_input:ident using $enum_projector:ident {
                        unit_variants {
                            $(
                                $unit_variant:ident as $unit_wire:literal;
                            )*
                        }
                        tuple_from_struct_variants {
                            $(
                                (
                                    $tuple_variant:ident as $tuple_wire:literal,
                                    $tuple_field:ident : $tuple_type:ty =
                                        $tuple_projection:ident
                                        $(($tuple_projector:ident))?,
                                    $tuple_order:literal,
                                    $tuple_null_empty:literal,
                                    $tuple_branch:literal $(,)?
                                );
                            )*
                        }
                        struct_variants {
                            $(
                                (
                                    $enum_variant:ident as $enum_wire:literal {
                                        $(
                                            (
                                                $enum_field:ident : $enum_type:ty =
                                                    $enum_projection:ident
                                                    $(($enum_member_projector:ident))?,
                                                $enum_order:literal,
                                                $enum_null_empty:literal,
                                                $enum_branch:literal $(,)?
                                            );
                                        )*
                                    }
                                );
                            )*
                        }
                    }
                )*
            }
        }
        typed_value_projection {
            transparent $typed_wrapper:ident wraps $typed_material:ident
                from $typed_input:ident using $typed_builder:ident
                exposed_as $typed_public_builder:ident {
                unit_variants {
                    $(
                        (
                            $typed_unit_owner:literal,
                            $typed_unit_normalized:literal,
                            $typed_unit_variant:ident as
                                $typed_unit_wire:literal,
                            route {
                                probes {
                                    $(
                                        $typed_unit_probe:ident;
                                    )+
                                }
                                mount $typed_unit_mount:ident;
                            } $(,)?
                        );
                    )*
                }
                direct_tuple_variants {
                    $(
                        (
                            $typed_direct_owner:literal,
                            $typed_direct_normalized:literal,
                            $typed_direct_variant:ident as
                                $typed_direct_wire:literal,
                            $typed_direct_value_owner:literal,
                            $typed_direct_value_normalized:literal,
                            $typed_direct_field:ident : $typed_direct_type:ty =
                                $typed_direct_projection:ident
                                $(($typed_direct_projector:ident))?,
                            $typed_direct_order:literal,
                            $typed_direct_null_empty:literal,
                            $typed_direct_branch:literal,
                            routes {
                                kind {
                                    probes {
                                        $(
                                            $typed_direct_kind_probe:ident;
                                        )+
                                    }
                                    mount
                                        $typed_direct_kind_mount:ident;
                                }
                                value {
                                    probes {
                                        $(
                                            $typed_direct_value_probe:ident;
                                        )+
                                    }
                                    mount
                                        $typed_direct_value_mount:ident;
                                }
                            } $(,)?
                        );
                    )*
                }
                number_tuple_variants {
                    $(
                        (
                            $typed_number_owner:literal,
                            $typed_number_normalized:literal,
                            $typed_number_variant:ident as
                                $typed_number_wire:literal,
                            $typed_number_value_owner:literal,
                            $typed_number_value_normalized:literal,
                            $typed_number_field:ident : $typed_number_type:ty =
                                $typed_number_projection:ident
                                $(($typed_number_projector:ident))?,
                            $typed_number_order:literal,
                            $typed_number_null_empty:literal,
                            $typed_number_branch:literal,
                            routes {
                                kind {
                                    probes {
                                        $(
                                            $typed_number_kind_probe:ident;
                                        )+
                                    }
                                    mount
                                        $typed_number_kind_mount:ident;
                                }
                                value {
                                    probes {
                                        $(
                                            $typed_number_value_probe:ident;
                                        )+
                                    }
                                    mount
                                        $typed_number_value_mount:ident;
                                }
                            } $(,)?
                        );
                    )*
                }
                struct_variant {
                    (
                        $typed_struct_owner:literal,
                        $typed_struct_normalized:literal,
                        $typed_struct_variant:ident as
                            $typed_struct_wire:literal,
                        $typed_struct_value:ident {
                            $(
                                (
                                    $typed_struct_field_owner:literal,
                                    $typed_struct_field_normalized:literal,
                                    $typed_struct_field_wire:literal,
                                    $typed_struct_field:ident :
                                        $typed_struct_type:ty =
                                        $typed_struct_projection:ident
                                        $(($typed_struct_projector:ident))?,
                                    $typed_struct_order:literal,
                                    $typed_struct_null_empty:literal,
                                    $typed_struct_branch:literal,
                                    route {
                                        probes {
                                            $(
                                                $typed_struct_field_probe:ident;
                                            )+
                                        }
                                        mount
                                            $typed_struct_field_mount:ident;
                                    } $(,)?
                                );
                            )*
                        },
                        kind_route {
                            probes {
                                $(
                                    $typed_struct_kind_probe:ident;
                                )+
                            }
                            mount $typed_struct_kind_mount:ident;
                        } $(,)?
                    );
                }
            }
        }
        source_record_projection {
            root $source_root:ident from ($source_input:ty)
                using $source_builder:ident
                mounts $source_root_mounts:tt
            {
                $(
                    (
                        $source_root_owner:literal,
                        $source_root_path:literal,
                        $source_root_order:literal,
                        $source_root_null_empty:literal,
                        $source_root_branch:literal,
                        $source_root_member:literal,
                        $(#[$source_root_attr:meta])*
                        $source_root_field:ident : $source_root_type:ty =
                            $source_root_access:ident {
                                $($source_root_projection:tt)+
                            }
                        $(, $source_root_disposition:ident)? $(,)?
                    );
                )*
            }
            structs {
                $(
                    struct $source_struct:ident from ($source_struct_input:ty)
                        using $source_struct_projector:ident
                        mounts $source_struct_mounts:tt
                    {
                        $(
                            (
                                $source_struct_owner:literal,
                                $source_struct_path:literal,
                                $source_struct_order:literal,
                                $source_struct_null_empty:literal,
                                $source_struct_branch:literal,
                                $source_struct_member:literal,
                                $(#[$source_struct_attr:meta])*
                                $source_struct_field:ident :
                                    $source_struct_type:ty =
                                    $source_struct_access:ident {
                                        $($source_struct_projection:tt)+
                                    }
                                $(, $source_struct_disposition:ident)? $(,)?
                            );
                        )*
                        $(
                            singleton_enum_fields {
                                $(
                                    (
                                        $source_singleton_owner:literal,
                                        $source_singleton_path:literal,
                                        $source_singleton_order:literal,
                                        $source_singleton_null_empty:literal,
                                        $source_singleton_branch:literal,
                                        $source_singleton_member:literal,
                                        $source_singleton_field:ident :
                                            $source_singleton_material:ident
                                            from
                                            ($source_singleton_input:path)
                                            using
                                            $source_singleton_projector:ident
                                            via
                                            $source_singleton_accessor:ident {
                                                $(
                                                    $source_singleton_variant:ident
                                                        as
                                                        $source_singleton_wire:literal;
                                                )*
                                            }
                                        $(, $source_singleton_disposition:ident)?
                                        $(,)?
                                    );
                                )*
                            }
                        )?
                    }
                )*
            }
            tagged_enums {
                $(
                    enum $source_enum:ident from ($source_enum_input:path)
                        using $source_enum_projector:ident
                        mounts $source_enum_mounts:tt
                    {
                        unit_variants {
                            $(
                                (
                                    $source_unit_owner:literal,
                                    $source_unit_path:literal,
                                    $source_unit_order:literal,
                                    $source_unit_null_empty:literal,
                                    $source_unit_branch:literal,
                                    $source_unit_member:literal,
                                    $source_unit_variant:ident as
                                        $source_unit_wire:literal
                                    $(, $source_unit_disposition:ident)? $(,)?
                                );
                            )*
                        }
                        tuple_variants {
                            $(
                                (
                                    $source_tuple_owner:literal,
                                    $source_tuple_path:literal,
                                    $source_tuple_order:literal,
                                    $source_tuple_null_empty:literal,
                                    $source_tuple_branch:literal,
                                    $source_tuple_member:literal,
                                    $source_tuple_variant:ident as
                                        $source_tuple_wire:literal
                                    ($source_tuple_type:ty) =
                                        $source_tuple_projector:ident
                                    $(, $source_tuple_disposition:ident)? $(,)?
                                );
                            )*
                        }
                        struct_variants {
                            $(
                                (
                                    $source_variant_owner:literal,
                                    $source_variant_path:literal,
                                    $source_variant_order:literal,
                                    $source_variant_null_empty:literal,
                                    $source_variant_branch:literal,
                                    $source_variant_member:literal,
                                    $source_variant:ident as
                                        $source_variant_wire:literal {
                                        $(
                                            (
                                                $source_enum_field_owner:literal,
                                                $source_enum_field_path:literal,
                                                $source_enum_field_order:literal,
                                                $source_enum_field_null_empty:literal,
                                                $source_enum_field_branch:literal,
                                                $source_enum_field_member:literal,
                                                $(#[$source_enum_field_attr:meta])*
                                                $source_enum_field:ident :
                                                    $source_enum_field_type:ty = {
                                                    $($source_enum_projection:tt)+
                                                }
                                                $(, $source_enum_field_disposition:ident)?
                                                $(,)?
                                            );
                                        )*
                                    }
                                    $(, $source_variant_disposition:ident)? $(,)?
                                );
                            )*
                        }
                    }
                )*
            }
        }
        semantic_projection {
            root pub struct $semantic_root:ident
                using $semantic_root_builder:ident(
                    $semantic_checked:ident,
                    $semantic_manifest:ident,
                    $semantic_root_context:ident
                ) {
                derived {
                    (
                        $semantic_derived_owner:literal,
                        $semantic_derived_normalized_member:literal,
                        $semantic_derived_source:ident,
                        $semantic_derived_path:literal,
                        $semantic_derived_class:literal,
                        $semantic_derived_order:literal,
                        $semantic_derived_null_empty:literal,
                        $semantic_derived_branch:literal,
                        $semantic_derived_member:literal,
                        $semantic_derived_field:ident :
                            $semantic_derived_type:ty =
                            $semantic_derived_expression:block $(,)?
                    );
                }
                fields {
                    $(
                        (
                            $semantic_root_owner:literal,
                            $semantic_root_normalized_member:literal,
                            $semantic_root_normalized_source:ident,
                            $semantic_root_path:literal,
                            $semantic_root_class:literal,
                            $semantic_root_order:literal,
                            $semantic_root_null_empty:literal,
                            $semantic_root_branch:literal,
                            $semantic_root_member:literal,
                            $(#[$semantic_root_field_attr:meta])*
                            $semantic_root_field:ident :
                                $semantic_root_type:ty =
                                $semantic_root_expression:block $(,)?
                        );
                    )*
                }
                composite_fields {
                    $(
                        (
                            $(#[$semantic_root_composite_attr:meta])*
                            $semantic_root_composite_field:ident :
                                $semantic_root_composite_type:ty =
                                $semantic_root_composite_expression:block
                                $(,)?
                        );
                    )*
                }
            }
            structs {
                $(
                    $(#[$semantic_struct_attr:meta])*
                    $semantic_struct_visibility:vis
                    struct $semantic_struct:ident
                        from ($semantic_struct_input:ty)
                        using $semantic_struct_projector:ident(
                            $semantic_struct_value:ident
                            $(
                                ;
                                $semantic_struct_context:ident :
                                    $semantic_struct_context_type:ty =
                                    $semantic_struct_context_expression:block
                            )?
                        ) {
                        $(
                            (
                                $semantic_struct_owner:literal,
                                $semantic_struct_normalized_member:literal,
                                $semantic_struct_normalized_source:ident,
                                $semantic_struct_path:literal,
                                $semantic_struct_class:literal,
                                $semantic_struct_order:literal,
                                $semantic_struct_null_empty:literal,
                                $semantic_struct_branch:literal,
                                $semantic_struct_member:literal,
                                $(#[$semantic_struct_field_attr:meta])*
                                $semantic_struct_field:ident :
                                    $semantic_struct_type:ty =
                                    $semantic_struct_expression:block
                                    $(, $semantic_struct_disposition:ident)?
                                    $(,)?
                            );
                        )*
                    }
                )*
            }
            generic_structs {
                $(
                    $(#[$semantic_generic_attr:meta])*
                    struct $semantic_generic_struct:ident
                        from ($semantic_generic_input:ty)
                        using $semantic_generic_projector:ident
                            <$semantic_generic_parameter:ident>(
                                $semantic_generic_value:ident
                            )
                        where
                            $semantic_generic_parameter_where:ident :
                                $semantic_generic_bound:path
                        {
                            $(
                                (
                                    $semantic_generic_owner:literal,
                                    $semantic_generic_normalized_member:literal,
                                    $semantic_generic_normalized_source:ident,
                                    $semantic_generic_path:literal,
                                    $semantic_generic_class:literal,
                                    $semantic_generic_order:literal,
                                    $semantic_generic_null_empty:literal,
                                    $semantic_generic_branch:literal,
                                    $semantic_generic_member:literal,
                                    $(#[$semantic_generic_field_attr:meta])*
                                    $semantic_generic_field:ident :
                                        $semantic_generic_type:ty =
                                        $semantic_generic_expression:block
                                        $(,)?
                                );
                            )*
                        }
                )*
            }
            closed_structs {
                $(
                    $(#[$semantic_closed_attr:meta])*
                    $semantic_closed_visibility:vis
                    struct $semantic_closed_struct:ident
                        from ($semantic_closed_input:ty)
                        using $semantic_closed_projector:ident(
                            $semantic_closed_value:ident
                        ) {
                        $(
                            (
                                $semantic_closed_owner:literal,
                                $semantic_closed_normalized_member:literal,
                                $semantic_closed_normalized_source:ident,
                                $semantic_closed_path:literal,
                                $semantic_closed_class:literal,
                                $semantic_closed_order:literal,
                                $semantic_closed_null_empty:literal,
                                $semantic_closed_branch:literal,
                                $semantic_closed_member:literal,
                                $(#[$semantic_closed_field_attr:meta])*
                                $semantic_closed_field:ident :
                                    $semantic_closed_type:ty =
                                    $semantic_closed_expression:block $(,)?
                            );
                        )*
                    }
                )*
            }
            tagged_enums {
                $(
                    $(#[$semantic_enum_attr:meta])*
                    enum $semantic_enum:ident
                        from ($semantic_enum_input:path)
                        using $semantic_enum_projector:ident(
                            $semantic_enum_value:ident
                            $(
                                ;
                                $semantic_enum_context:ident :
                                    $semantic_enum_context_type:ty =
                                    $semantic_enum_context_expression:block
                            )?
                        ) {
                        unit_variants {
                            $(
                                (
                                    $semantic_unit_owner:literal,
                                    $semantic_unit_normalized_member:literal,
                                    $semantic_unit_normalized_source:ident,
                                    $semantic_unit_path:literal,
                                    $semantic_unit_class:literal,
                                    $semantic_unit_order:literal,
                                    $semantic_unit_null_empty:literal,
                                    $semantic_unit_branch:literal,
                                    $semantic_unit_member:literal,
                                    $semantic_unit_variant:ident as
                                        $semantic_unit_wire:literal $(,)?
                                );
                            )*
                        }
                        composite_unit_variants {
                            $(
                                $semantic_composite_unit_variant:ident as
                                    $semantic_composite_unit_wire:literal;
                            )*
                        }
                        tuple_variants {
                            $(
                                (
                                    $semantic_tuple_owner:literal,
                                    $semantic_tuple_normalized_member:literal,
                                    $semantic_tuple_normalized_source:ident,
                                    $semantic_tuple_path:literal,
                                    $semantic_tuple_class:literal,
                                    $semantic_tuple_order:literal,
                                    $semantic_tuple_null_empty:literal,
                                    $semantic_tuple_branch:literal,
                                    $semantic_tuple_member:literal,
                                    $semantic_tuple_variant:ident as
                                        $semantic_tuple_wire:literal
                                    (
                                        $semantic_tuple_value:ident :
                                            $semantic_tuple_type:ty =
                                            $semantic_tuple_expression:block
                                    ) $(,)?
                                );
                            )*
                        }
                        tuple_struct_variants {
                            $(
                                (
                                    $semantic_tuple_struct_owner:literal,
                                    $semantic_tuple_struct_normalized_member:literal,
                                    $semantic_tuple_struct_normalized_source:ident,
                                    $semantic_tuple_struct_path:literal,
                                    $semantic_tuple_struct_class:literal,
                                    $semantic_tuple_struct_order:literal,
                                    $semantic_tuple_struct_null_empty:literal,
                                    $semantic_tuple_struct_branch:literal,
                                    $semantic_tuple_struct_member:literal,
                                    $semantic_tuple_struct_variant:ident as
                                        $semantic_tuple_struct_wire:literal
                                    (
                                        $semantic_tuple_struct_value:ident :
                                            $semantic_tuple_struct_type:ty =
                                            $semantic_tuple_struct_expression:block
                                    ) {
                                        $(
                                            (
                                                $semantic_tuple_struct_field_owner:literal,
                                                $semantic_tuple_struct_field_normalized_member:literal,
                                                $semantic_tuple_struct_field_normalized_source:ident,
                                                $semantic_tuple_struct_field_path:literal,
                                                $semantic_tuple_struct_field_class:literal,
                                                $semantic_tuple_struct_field_order:literal,
                                                $semantic_tuple_struct_field_null_empty:literal,
                                                $semantic_tuple_struct_field_branch:literal,
                                                $semantic_tuple_struct_field_member:literal,
                                                $(,)?
                                            );
                                        )*
                                    } $(,)?
                                );
                            )*
                        }
                        struct_variants {
                            $(
                                (
                                    $semantic_variant_owner:literal,
                                    $semantic_variant_normalized_member:literal,
                                    $semantic_variant_normalized_source:ident,
                                    $semantic_variant_path:literal,
                                    $semantic_variant_class:literal,
                                    $semantic_variant_order:literal,
                                    $semantic_variant_null_empty:literal,
                                    $semantic_variant_branch:literal,
                                    $semantic_variant_member:literal,
                                    $semantic_variant:ident as
                                        $semantic_variant_wire:literal {
                                        $(
                                            (
                                                $semantic_enum_field_owner:literal,
                                                $semantic_enum_field_normalized_member:literal,
                                                $semantic_enum_field_normalized_source:ident,
                                                $semantic_enum_field_path:literal,
                                                $semantic_enum_field_class:literal,
                                                $semantic_enum_field_order:literal,
                                                $semantic_enum_field_null_empty:literal,
                                                $semantic_enum_field_branch:literal,
                                                $semantic_enum_field_member:literal,
                                                $(#[$semantic_enum_field_attr:meta])*
                                                $semantic_enum_field:ident :
                                                    $semantic_enum_field_type:ty =
                                                    $semantic_enum_field_expression:block
                                                    $(,)?
                                            );
                                        )*
                                    } $(,)?
                                );
                            )*
                        }
                    }
                )*
            }
            singleton_enums {
                $(
                    $(#[$semantic_singleton_attr:meta])*
                    enum $semantic_singleton:ident
                        from ($semantic_singleton_input:ty)
                        using $semantic_singleton_projector:ident(
                            $semantic_singleton_value:ident
                        ) {
                        $semantic_singleton_variant:ident as
                            $semantic_singleton_wire:literal
                    }
                )*
            }
        }
        provenance_projection {
            owner_paths {
                $(
                    (
                        $provenance_owner:literal,
                        $provenance_normalized_member:literal,
                        $provenance_normalized_source:ident,
                        $provenance_domain:literal,
                        $provenance_path:literal,
                        $provenance_class:literal,
                        $provenance_order:literal,
                        $provenance_null_empty:literal,
                        $provenance_branch:literal,
                        $provenance_member:literal,
                        $provenance_material_source:ident,
                        $provenance_case:ident,
                        $provenance_disposition:ident $(,)?
                    );
                )*
            }
            context struct $provenance_context:ident
                using $provenance_context_validator:ident {
                parameters {
                    $(
                        $provenance_parameter:ident :
                            $provenance_parameter_type:ty;
                    )*
                }
                derived_fields {
                    $(
                        $provenance_context_field:ident :
                            $provenance_context_field_type:ty;
                    )*
                }
                validate(
                    $provenance_context_checked:ident,
                    $provenance_context_value:ident
                ) $provenance_context_validation:block
            }
            normalized_contexts {
                $(
                    struct $provenance_normalized_struct:ident {
                        $(
                            $provenance_normalized_struct_field:ident :
                                $provenance_normalized_struct_type:ty;
                        )*
                    }
                )*
                $(
                    enum $provenance_normalized_enum:ident {
                        $(
                            $provenance_normalized_variant:ident {
                                $(
                                    $provenance_normalized_variant_field:ident :
                                        $provenance_normalized_variant_type:ty;
                                )*
                            }
                        )*
                    }
                )*
            }
            root pub struct $provenance_root:ident
                using $provenance_builder:ident(
                    $provenance_checked:ident,
                    $provenance_root_context:ident
                ) {
                $(
                    (
                        $(#[$provenance_root_field_attr:meta])*
                        $provenance_root_field:ident :
                            $provenance_root_field_type:ty =
                            $provenance_root_field_expression:block $(,)?
                    );
                )*
            }
            structs {
                $(
                    $(#[$provenance_struct_attr:meta])*
                    $provenance_struct_visibility:vis
                    struct $provenance_struct:ident
                        using $provenance_struct_projector:ident(
                            $(
                                $provenance_struct_argument:ident :
                                    $provenance_struct_argument_type:ty
                            ),* $(,)?
                        ) {
                        $(
                            (
                                $(#[$provenance_struct_field_attr:meta])*
                                $provenance_struct_field:ident :
                                    $provenance_struct_field_type:ty =
                                    $provenance_struct_field_expression:block
                                    $(,)?
                            );
                        )*
                    }
                )*
            }
            tagged_enums {
                $(
                    enum $provenance_enum:ident
                        from ($provenance_enum_input:path)
                        using $provenance_enum_projector:ident(
                            $provenance_enum_value:ident
                        ) {
                        $(
                            $provenance_enum_variant:ident as
                                $provenance_enum_wire:literal {
                                $(
                                    (
                                        $provenance_enum_field:ident :
                                            $provenance_enum_field_type:ty =
                                            $provenance_enum_field_expression:block
                                            $(,)?
                                    );
                                )*
                            }
                        )*
                    }
                )*
            }
            derived_dependencies {
                $(
                    (
                        $provenance_dependency_input:literal,
                        $provenance_dependency_member:literal,
                        $provenance_dependency_rule:literal $(,)?
                    );
                )*
            }
        }
        $($declaration:item)*
    ) => {
        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        pub struct $value_material {
            $(
                $value_field: $value_type,
            )*
            $(
                $derived_field: $derived_type,
            )*
        }

        struct $value_context<'a> {
            value: &'a ValueContractCatalog,
            $(
                $derived_field: $derived_type,
            )*
        }

        pub fn $value_builder(
            value: &ValueContractCatalog,
            $(
                $derived_field: $derived_type,
            )*
        ) -> Result<$value_material, CatalogError> {
            let context = $value_context {
                value,
                $(
                    $derived_field: validate_value_contract_parameter!(
                        $derived_field,
                        $derived_validator
                    )?,
                )*
            };
            Ok($value_material {
                $(
                    $value_field: project_value_contract_value!(
                        &context.value.$value_field,
                        $value_projection $(($value_projector))?
                    )?,
                )*
                $(
                    $derived_field: context.$derived_field,
                )*
            })
        }

        $(
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(deny_unknown_fields)]
            struct $struct_material {
                $(
                    $struct_field: $struct_type,
                )*
            }

            fn $struct_projector(
                value: &$struct_input,
            ) -> Result<$struct_material, CatalogError> {
                Ok($struct_material {
                    $(
                        $struct_field: project_value_contract_value!(
                            &value.$struct_field,
                            $struct_projection $(($struct_member_projector))?
                        )?,
                    )*
                })
            }
        )*

        $(
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(transparent)]
            struct $enum_wrapper($enum_material);

            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(
                deny_unknown_fields,
                tag = "kind",
                content = "value",
                rename_all = "snake_case"
            )]
            enum $enum_material {
                $(
                    #[serde(rename = $unit_wire)]
                    $unit_variant(()),
                )*
                $(
                    #[serde(rename = $tuple_wire)]
                    $tuple_variant($tuple_type),
                )*
                $(
                    #[serde(rename = $enum_wire)]
                    $enum_variant {
                        $(
                            $enum_field: $enum_type,
                        )*
                    },
                )*
            }

            fn $enum_projector(
                value: &$enum_input,
            ) -> Result<$enum_wrapper, CatalogError> {
                let material = match value {
                    $(
                        $enum_input::$unit_variant => {
                            $enum_material::$unit_variant(())
                        }
                    )*
                    $(
                        $enum_input::$tuple_variant { $tuple_field } => {
                            $enum_material::$tuple_variant(
                                project_value_contract_value!(
                                    $tuple_field,
                                    $tuple_projection $(($tuple_projector))?
                                )?,
                            )
                        }
                    )*
                    $(
                        $enum_input::$enum_variant {
                            $(
                                $enum_field,
                            )*
                        } => {
                            $enum_material::$enum_variant {
                                $(
                                    $enum_field: project_value_contract_value!(
                                        $enum_field,
                                        $enum_projection
                                            $(($enum_member_projector))?
                                    )?,
                                )*
                            }
                        }
                    )*
                };
                Ok($enum_wrapper(material))
            }
        )*

        #[derive(Clone, Debug, Eq, PartialEq)]
        /// Closed typed-value projection material.
        ///
        /// Use the checked constructors or projection builder; raw JSON
        /// deserialization is intentionally unavailable.
        ///
        /// ```compile_fail
        /// use donat_connector_catalog::TypedValueMaterialV1;
        /// let _: TypedValueMaterialV1 =
        ///     serde_json::from_str(
        ///         r#"{"kind":"i64","value":"not-an-integer"}"#
        ///     ).unwrap();
        /// ```
        pub struct $typed_wrapper(pub(crate) $typed_material);

        #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(
            deny_unknown_fields,
            tag = "kind",
            content = "value",
            rename_all = "snake_case"
        )]
        pub(crate) enum $typed_material {
            $(
                #[serde(rename = $typed_unit_wire)]
                $typed_unit_variant,
            )*
            $(
                #[serde(rename = $typed_direct_wire)]
                $typed_direct_variant($typed_direct_type),
            )*
            $(
                #[serde(rename = $typed_number_wire)]
                $typed_number_variant($typed_number_type),
            )*
            #[serde(rename = $typed_struct_wire)]
            $typed_struct_variant {
                $(
                    #[serde(rename = $typed_struct_field_wire)]
                    $typed_struct_field: $typed_struct_type,
                )*
            },
        }

        impl $typed_wrapper {
            pub(crate) fn $typed_builder(value: &$typed_input) -> Self {
                fn project(value: &$typed_input) -> $typed_material {
                    match value {
                        $(
                            $typed_input::$typed_unit_variant => {
                                $typed_material::$typed_unit_variant
                            }
                        )*
                        $(
                            $typed_input::$typed_direct_variant(
                                $typed_direct_field,
                            ) => {
                                $typed_material::$typed_direct_variant(
                                    project_typed_value!(
                                        $typed_direct_field,
                                        $typed_direct_projection
                                            $(($typed_direct_projector))?
                                    ),
                                )
                            }
                        )*
                        $(
                            $typed_input::Number(
                                CanonicalNumber::$typed_number_variant(
                                    $typed_number_field,
                                ),
                            ) => {
                                $typed_material::$typed_number_variant(
                                    project_typed_value!(
                                        $typed_number_field,
                                        $typed_number_projection
                                            $(($typed_number_projector))?
                                    ),
                                )
                            }
                        )*
                        $typed_input::$typed_struct_variant(
                            $typed_struct_value,
                        ) => {
                            $typed_material::$typed_struct_variant {
                                $(
                                    $typed_struct_field:
                                        project_typed_value!(
                                            $typed_struct_value,
                                            $typed_struct_projection
                                                $(($typed_struct_projector))?
                                        ),
                                )*
                            }
                        }
                    }
                }
                Self(project(value))
            }
        }

        pub fn $typed_public_builder(value: &$typed_input) -> $typed_wrapper {
            $typed_wrapper::$typed_builder(value)
        }

        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        /// Closed source-record hash material.
        ///
        /// Raw JSON cannot construct this permanent hash input.
        ///
        /// ```compile_fail
        /// use donat_connector_catalog::SourceRecordMaterialV1;
        /// let _: SourceRecordMaterialV1 =
        ///     serde_json::from_str("{}").unwrap();
        /// ```
        pub struct $source_root {
            $(
                $(#[$source_root_attr])*
                $source_root_field: $source_root_type,
            )*
        }

        pub fn $source_builder(
            value: &$source_input,
        ) -> Result<$source_root, CatalogError> {
            validate_source_projection_input(value)?;
            Ok($source_root {
                $(
                    $source_root_field: project_source_field!(
                        value,
                        $source_root_field,
                        $source_root_access {
                            $($source_root_projection)+
                        }
                    )?,
                )*
            })
        }

        $(
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(deny_unknown_fields)]
            struct $source_struct {
                $(
                    $(#[$source_struct_attr])*
                    $source_struct_field: $source_struct_type,
                )*
                $(
                    $(
                        $source_singleton_field:
                            $source_singleton_material,
                    )*
                )?
            }

            fn $source_struct_projector(
                value: &$source_struct_input,
            ) -> Result<$source_struct, CatalogError> {
                Ok($source_struct {
                    $(
                        $source_struct_field: project_source_field!(
                            value,
                            $source_struct_field,
                            $source_struct_access {
                                $($source_struct_projection)+
                            }
                        )?,
                    )*
                    $(
                        $(
                            $source_singleton_field:
                                $source_singleton_projector(
                                    &value.$source_singleton_accessor(),
                                )?,
                        )*
                    )?
                })
            }

            $(
                $(
                    #[derive(
                        Clone,
                        Debug,
                        Deserialize,
                        Eq,
                        PartialEq,
                        Serialize
                    )]
                    #[serde(
                        deny_unknown_fields,
                        tag = "kind",
                        content = "value",
                        rename_all = "snake_case"
                    )]
                    enum $source_singleton_material {
                        $(
                            #[serde(rename = $source_singleton_wire)]
                            $source_singleton_variant(()),
                        )*
                    }

                    fn $source_singleton_projector(
                        value: &$source_singleton_input,
                    ) -> Result<$source_singleton_material, CatalogError> {
                        use $source_singleton_input as SourceSingletonInput;

                        Ok(match value {
                            $(
                                SourceSingletonInput::
                                    $source_singleton_variant => {
                                    $source_singleton_material::
                                        $source_singleton_variant(())
                                }
                            )*
                        })
                    }
                )*
            )?
        )*

        $(
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(
                deny_unknown_fields,
                tag = "kind",
                content = "value",
                rename_all = "snake_case"
            )]
            #[allow(clippy::large_enum_variant)]
            enum $source_enum {
                $(
                    #[serde(rename = $source_unit_wire)]
                    $source_unit_variant(()),
                )*
                $(
                    #[serde(rename = $source_tuple_wire)]
                    $source_tuple_variant($source_tuple_type),
                )*
                $(
                    #[serde(rename = $source_variant_wire)]
                    $source_variant {
                        $(
                            $(#[$source_enum_field_attr])*
                            $source_enum_field: $source_enum_field_type,
                        )*
                    },
                )*
            }

            fn $source_enum_projector(
                value: &$source_enum_input,
            ) -> Result<$source_enum, CatalogError> {
                use $source_enum_input as SourceProjectionInput;

                Ok(match value {
                    $(
                        SourceProjectionInput::$source_unit_variant => {
                            $source_enum::$source_unit_variant(())
                        }
                    )*
                    $(
                        SourceProjectionInput::$source_tuple_variant(payload) => {
                            $source_enum::$source_tuple_variant(
                                $source_tuple_projector(payload)?,
                            )
                        }
                    )*
                    $(
                        SourceProjectionInput::$source_variant {
                            $(
                                $source_enum_field,
                            )*
                        } => {
                            $source_enum::$source_variant {
                                $(
                                    $source_enum_field: project_source_value!(
                                        $source_enum_field,
                                        $($source_enum_projection)+
                                    )?,
                                )*
                            }
                        }
                    )*
                })
            }
        )*

        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        /// Closed semantic hash material produced only from a checked manifest.
        ///
        /// Nested semantic operation material is intentionally not public.
        ///
        /// ```compile_fail
        /// use donat_connector_catalog::SemanticOperationMaterialV1;
        /// ```
        pub struct $semantic_root {
            $semantic_derived_field: $semantic_derived_type,
            $(
                $(#[$semantic_root_field_attr])*
                $semantic_root_field: $semantic_root_type,
            )*
            $(
                $(#[$semantic_root_composite_attr])*
                $semantic_root_composite_field:
                    $semantic_root_composite_type,
            )*
        }

        pub fn $semantic_root_builder(
            $semantic_checked: &crate::CheckedConnectorManifest<'_>,
            $semantic_root_context: $semantic_derived_type,
        ) -> Result<$semantic_root, CatalogError> {
            let $semantic_manifest = $semantic_checked.manifest();
            let $semantic_derived_field = $semantic_derived_expression;
            Ok($semantic_root {
                $semantic_derived_field,
                $(
                    $semantic_root_field: $semantic_root_expression,
                )*
                $(
                    $semantic_root_composite_field:
                        $semantic_root_composite_expression,
                )*
            })
        }

        $(
            $(#[$semantic_struct_attr])*
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(deny_unknown_fields)]
            $semantic_struct_visibility struct $semantic_struct {
                $(
                    $(#[$semantic_struct_field_attr])*
                    $semantic_struct_field: $semantic_struct_type,
                )*
            }

            fn $semantic_struct_projector(
                $semantic_struct_value: &$semantic_struct_input
                $(
                    ,
                    $semantic_struct_context:
                        $semantic_struct_context_type
                )?
            ) -> Result<$semantic_struct, CatalogError> {
                $(
                    let $semantic_struct_context =
                        $semantic_struct_context_expression;
                )?
                Ok($semantic_struct {
                    $(
                        $semantic_struct_field:
                            $semantic_struct_expression,
                    )*
                })
            }
        )*

        $(
            $(#[$semantic_generic_attr])*
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(deny_unknown_fields)]
            struct $semantic_generic_struct {
                $(
                    $(#[$semantic_generic_field_attr])*
                    $semantic_generic_field: $semantic_generic_type,
                )*
            }

            fn $semantic_generic_projector<$semantic_generic_parameter>(
                $semantic_generic_value: &$semantic_generic_input,
            ) -> Result<$semantic_generic_struct, CatalogError>
            where
                $semantic_generic_parameter: $semantic_generic_bound,
            {
                Ok($semantic_generic_struct {
                    $(
                        $semantic_generic_field:
                            $semantic_generic_expression,
                    )*
                })
            }
        )*

        $(
            $(#[$semantic_closed_attr])*
            #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
            #[serde(deny_unknown_fields)]
            $semantic_closed_visibility struct $semantic_closed_struct {
                $(
                    $(#[$semantic_closed_field_attr])*
                    $semantic_closed_field: $semantic_closed_type,
                )*
            }

            fn $semantic_closed_projector(
                $semantic_closed_value: &$semantic_closed_input,
            ) -> Result<$semantic_closed_struct, CatalogError> {
                Ok($semantic_closed_struct {
                    $(
                        $semantic_closed_field:
                            $semantic_closed_expression,
                    )*
                })
            }
        )*

        $(
            $(#[$semantic_enum_attr])*
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(
                deny_unknown_fields,
                tag = "kind",
                content = "value",
                rename_all = "snake_case"
            )]
            enum $semantic_enum {
                $(
                    #[serde(rename = $semantic_unit_wire)]
                    $semantic_unit_variant(()),
                )*
                $(
                    #[serde(rename = $semantic_composite_unit_wire)]
                    $semantic_composite_unit_variant(()),
                )*
                $(
                    #[serde(rename = $semantic_tuple_wire)]
                    $semantic_tuple_variant($semantic_tuple_type),
                )*
                $(
                    #[serde(rename = $semantic_tuple_struct_wire)]
                    $semantic_tuple_struct_variant(
                        $semantic_tuple_struct_type
                    ),
                )*
                $(
                    #[serde(rename = $semantic_variant_wire)]
                    $semantic_variant {
                        $(
                            $(#[$semantic_enum_field_attr])*
                            $semantic_enum_field:
                                $semantic_enum_field_type,
                        )*
                    },
                )*
            }

            fn $semantic_enum_projector(
                $semantic_enum_value: &$semantic_enum_input
                $(
                    ,
                    $semantic_enum_context: $semantic_enum_context_type
                )?
            ) -> Result<$semantic_enum, CatalogError> {
                use $semantic_enum_input as SemanticProjectionInput;

                $(
                    let $semantic_enum_context =
                        $semantic_enum_context_expression;
                )?
                Ok(match $semantic_enum_value {
                    $(
                        SemanticProjectionInput::$semantic_unit_variant => {
                            $semantic_enum::$semantic_unit_variant(())
                        }
                    )*
                    $(
                        SemanticProjectionInput::
                            $semantic_composite_unit_variant => {
                            $semantic_enum::
                                $semantic_composite_unit_variant(())
                        }
                    )*
                    $(
                        SemanticProjectionInput::$semantic_tuple_variant(
                            $semantic_tuple_value,
                        ) => {
                            $semantic_enum::$semantic_tuple_variant(
                                $semantic_tuple_expression,
                            )
                        }
                    )*
                    $(
                        SemanticProjectionInput::
                            $semantic_tuple_struct_variant(
                                $semantic_tuple_struct_value,
                            ) => {
                            $semantic_enum::
                                $semantic_tuple_struct_variant(
                                    $semantic_tuple_struct_expression,
                                )
                        }
                    )*
                    $(
                        SemanticProjectionInput::$semantic_variant {
                            $(
                                $semantic_enum_field,
                            )*
                        } => {
                            $semantic_enum::$semantic_variant {
                                $(
                                    $semantic_enum_field:
                                        $semantic_enum_field_expression,
                                )*
                            }
                        }
                    )*
                })
            }
        )*

        $(
            $(#[$semantic_singleton_attr])*
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(
                deny_unknown_fields,
                tag = "kind",
                content = "value",
                rename_all = "snake_case"
            )]
            enum $semantic_singleton {
                #[serde(rename = $semantic_singleton_wire)]
                $semantic_singleton_variant(()),
            }

            fn $semantic_singleton_projector(
                $semantic_singleton_value: &$semantic_singleton_input,
            ) -> Result<$semantic_singleton, CatalogError> {
                let _ = $semantic_singleton_value;
                Ok($semantic_singleton::$semantic_singleton_variant(()))
            }
        )*

        struct $provenance_context<'view, 'catalog> {
            checked:
                &'view crate::CheckedConnectorManifest<'catalog>,
            $(
                $provenance_parameter:
                    $provenance_parameter_type,
            )*
            $(
                $provenance_context_field:
                    $provenance_context_field_type,
            )*
        }

        fn $provenance_context_validator<'view, 'catalog>(
            $provenance_context_checked:
                &'view crate::CheckedConnectorManifest<'catalog>,
            $(
                $provenance_parameter:
                    $provenance_parameter_type,
            )*
        ) -> Result<
            $provenance_context<'view, 'catalog>,
            CatalogError,
        > {
            let $provenance_context_value =
                $provenance_context_checked.manifest();
            $provenance_context_validation
        }

        $(
            #[derive(Clone, Debug)]
            struct $provenance_normalized_struct {
                $(
                    $provenance_normalized_struct_field:
                        $provenance_normalized_struct_type,
                )*
            }
        )*

        $(
            #[derive(Clone, Debug)]
            enum $provenance_normalized_enum {
                $(
                    $provenance_normalized_variant {
                        $(
                            $provenance_normalized_variant_field:
                                $provenance_normalized_variant_type,
                        )*
                    },
                )*
            }
        )*

        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        pub struct $provenance_root {
            $(
                $(#[$provenance_root_field_attr])*
                $provenance_root_field:
                    $provenance_root_field_type,
            )*
        }

        /// Builds provenance from one checked compilation proof.
        ///
        /// The checked proof already borrows the exact accepted-record and
        /// reviewed-policy contexts used by compilation. Neither contradictory
        /// contexts nor a caller-selected semantic hash are accepted by this
        /// API.
        ///
        /// ```compile_fail
        /// use std::collections::BTreeMap;
        /// use donat_connector_catalog::{
        ///     provenance_material, AcceptedRecordCatalog,
        ///     CheckedConnectorManifest, DonatPolicyId,
        /// };
        /// use donat_value_contract::TypedValue;
        ///
        /// fn forge(
        ///     checked: &CheckedConnectorManifest<'_>,
        ///     other_catalog: &AcceptedRecordCatalog,
        ///     other_policies: &BTreeMap<DonatPolicyId, TypedValue>,
        /// ) {
        ///     let claimed_semantic_hash = [0xff; 32];
        ///     let _ = provenance_material(
        ///         checked,
        ///         other_catalog,
        ///         other_policies,
        ///         claimed_semantic_hash,
        ///         1,
        ///         1,
        ///         1,
        ///     );
        /// }
        /// ```
        pub fn $provenance_builder(
            $provenance_checked:
                &crate::CheckedConnectorManifest<'_>,
            $(
                $provenance_parameter:
                    $provenance_parameter_type,
            )*
        ) -> Result<$provenance_root, CatalogError> {
            let $provenance_root_context =
                $provenance_context_validator(
                    $provenance_checked,
                    $(
                        $provenance_parameter,
                    )*
                )?;
            Ok($provenance_root {
                $(
                    $provenance_root_field:
                        $provenance_root_field_expression,
                )*
            })
        }

        $(
            $(#[$provenance_struct_attr])*
            #[derive(
                Clone,
                Debug,
                Deserialize,
                Eq,
                PartialEq,
                Serialize
            )]
            #[serde(deny_unknown_fields)]
            $provenance_struct_visibility struct $provenance_struct {
                $(
                    $(#[$provenance_struct_field_attr])*
                    $provenance_struct_field:
                        $provenance_struct_field_type,
                )*
            }

            fn $provenance_struct_projector(
                $(
                    $provenance_struct_argument:
                        $provenance_struct_argument_type,
                )*
            ) -> Result<$provenance_struct, CatalogError> {
                Ok($provenance_struct {
                    $(
                        $provenance_struct_field:
                            $provenance_struct_field_expression,
                    )*
                })
            }
        )*

        $(
            #[derive(
                Clone,
                Debug,
                Deserialize,
                Eq,
                PartialEq,
                Serialize
            )]
            #[serde(
                deny_unknown_fields,
                tag = "kind",
                content = "value",
                rename_all = "snake_case"
            )]
            enum $provenance_enum {
                $(
                    #[serde(rename = $provenance_enum_wire)]
                    $provenance_enum_variant {
                        $(
                            $provenance_enum_field:
                                $provenance_enum_field_type,
                        )*
                    },
                )*
            }

            fn $provenance_enum_projector(
                $provenance_enum_value:
                    &$provenance_enum_input,
            ) -> Result<$provenance_enum, CatalogError> {
                use $provenance_enum_input as ProvenanceProjectionInput;

                Ok(match $provenance_enum_value {
                    $(
                        ProvenanceProjectionInput::
                            $provenance_enum_variant {
                            $(
                                $provenance_enum_field,
                            )*
                        } => {
                            $provenance_enum::
                                $provenance_enum_variant {
                                $(
                                    $provenance_enum_field:
                                        $provenance_enum_field_expression,
                                )*
                            }
                        }
                    )*
                })
            }
        )*

        $($declaration)*

        /// The exact declaration descriptor that generated every closed
        /// canonical material type.
        pub const CANONICAL_PROJECTION_SCHEMA_DECLARATIONS: &str =
            concat!(
                stringify!(
                    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
                    pub struct $value_material {
                        $(
                            $value_field: $value_type,
                        )*
                        $(
                            $derived_field: $derived_type,
                        )*
                    }
                ),
                "\n",
                $(
                    stringify!(
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        struct $struct_material {
                            $(
                                $struct_field: $struct_type,
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(transparent)]
                        struct $enum_wrapper($enum_material);

                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(
                            deny_unknown_fields,
                            tag = "kind",
                            content = "value",
                            rename_all = "snake_case"
                        )]
                        enum $enum_material {
                            $(
                                #[serde(rename = $unit_wire)]
                                $unit_variant(()),
                            )*
                            $(
                                #[serde(rename = $tuple_wire)]
                                $tuple_variant($tuple_type),
                            )*
                            $(
                                #[serde(rename = $enum_wire)]
                                $enum_variant {
                                    $(
                                        $enum_field: $enum_type,
                                    )*
                                },
                            )*
                        }
                    ),
                    "\n",
                )*
                stringify!(
                    #[derive(Clone, Debug, Eq, PartialEq)]
                    pub struct $typed_wrapper($typed_material);

                    #[derive(
                        Clone,
                        Debug,
                        Deserialize,
                        Eq,
                        PartialEq,
                        Serialize
                    )]
                    #[serde(
                        deny_unknown_fields,
                        tag = "kind",
                        content = "value",
                        rename_all = "snake_case"
                    )]
                    enum $typed_material {
                        $(
                            #[serde(rename = $typed_unit_wire)]
                            $typed_unit_variant,
                        )*
                        $(
                            #[serde(rename = $typed_direct_wire)]
                            $typed_direct_variant($typed_direct_type),
                        )*
                        $(
                            #[serde(rename = $typed_number_wire)]
                            $typed_number_variant($typed_number_type),
                        )*
                        #[serde(rename = $typed_struct_wire)]
                        $typed_struct_variant {
                            $(
                                #[serde(rename = $typed_struct_field_wire)]
                                $typed_struct_field: $typed_struct_type,
                            )*
                        },
                    }
                ),
                "\n",
                stringify!(
                    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
                    #[serde(deny_unknown_fields)]
                    pub struct $source_root {
                        $(
                            $(#[$source_root_attr])*
                            $source_root_field: $source_root_type,
                        )*
                    }
                ),
                "\n",
                $(
                    stringify!(
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        struct $source_struct {
                            $(
                                $(#[$source_struct_attr])*
                                $source_struct_field: $source_struct_type,
                            )*
                            $(
                                $(
                                    $source_singleton_field:
                                        $source_singleton_material,
                                )*
                            )?
                        }
                    ),
                    "\n",
                    $(
                        $(
                            stringify!(
                                #[derive(
                                    Clone,
                                    Debug,
                                    Deserialize,
                                    Eq,
                                    PartialEq,
                                    Serialize
                                )]
                                #[serde(
                                    deny_unknown_fields,
                                    tag = "kind",
                                    content = "value",
                                    rename_all = "snake_case"
                                )]
                                enum $source_singleton_material {
                                    $(
                                        #[serde(
                                            rename =
                                                $source_singleton_wire
                                        )]
                                        $source_singleton_variant(()),
                                    )*
                                }
                            ),
                            "\n",
                        )*
                    )?
                )*
                $(
                    stringify!(
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(
                            deny_unknown_fields,
                            tag = "kind",
                            content = "value",
                            rename_all = "snake_case"
                        )]
                        enum $source_enum {
                            $(
                                #[serde(rename = $source_unit_wire)]
                                $source_unit_variant(()),
                            )*
                            $(
                                #[serde(rename = $source_tuple_wire)]
                                $source_tuple_variant($source_tuple_type),
                            )*
                            $(
                                #[serde(rename = $source_variant_wire)]
                                $source_variant {
                                    $(
                                        $(#[$source_enum_field_attr])*
                                        $source_enum_field:
                                            $source_enum_field_type,
                                    )*
                                },
                            )*
                        }
                    ),
                    "\n",
                )*
                stringify!(
                    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
                    #[serde(deny_unknown_fields)]
                    pub struct $semantic_root {
                        $semantic_derived_field: $semantic_derived_type,
                        $(
                            $(#[$semantic_root_field_attr])*
                            $semantic_root_field: $semantic_root_type,
                        )*
                        $(
                            $(#[$semantic_root_composite_attr])*
                            $semantic_root_composite_field:
                                $semantic_root_composite_type,
                        )*
                    }
                ),
                "\n",
                $(
                    stringify!(
                        $(#[$semantic_struct_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        $semantic_struct_visibility struct $semantic_struct {
                            $(
                                $(#[$semantic_struct_field_attr])*
                                $semantic_struct_field:
                                    $semantic_struct_type,
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        $(#[$semantic_generic_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        struct $semantic_generic_struct {
                            $(
                                $(#[$semantic_generic_field_attr])*
                                $semantic_generic_field:
                                    $semantic_generic_type,
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        $(#[$semantic_closed_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        $semantic_closed_visibility
                        struct $semantic_closed_struct {
                            $(
                                $(#[$semantic_closed_field_attr])*
                                $semantic_closed_field:
                                    $semantic_closed_type,
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        $(#[$semantic_enum_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(
                            deny_unknown_fields,
                            tag = "kind",
                            content = "value",
                            rename_all = "snake_case"
                        )]
                        enum $semantic_enum {
                            $(
                                #[serde(rename = $semantic_unit_wire)]
                                $semantic_unit_variant(()),
                            )*
                            $(
                                #[serde(
                                    rename =
                                        $semantic_composite_unit_wire
                                )]
                                $semantic_composite_unit_variant(()),
                            )*
                            $(
                                #[serde(rename = $semantic_tuple_wire)]
                                $semantic_tuple_variant(
                                    $semantic_tuple_type,
                                ),
                            )*
                            $(
                                #[serde(
                                    rename =
                                        $semantic_tuple_struct_wire
                                )]
                                $semantic_tuple_struct_variant(
                                    $semantic_tuple_struct_type
                                ),
                            )*
                            $(
                                #[serde(rename = $semantic_variant_wire)]
                                $semantic_variant {
                                    $(
                                        $(#[$semantic_enum_field_attr])*
                                        $semantic_enum_field:
                                            $semantic_enum_field_type,
                                    )*
                                },
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        $(#[$semantic_singleton_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(
                            deny_unknown_fields,
                            tag = "kind",
                            content = "value",
                            rename_all = "snake_case"
                        )]
                        enum $semantic_singleton {
                            #[serde(
                                rename = $semantic_singleton_wire
                            )]
                            $semantic_singleton_variant(()),
                        }
                    ),
                    "\n",
                )*
                stringify!(
                    #[derive(
                        Clone,
                        Debug,
                        Eq,
                        PartialEq,
                        Serialize
                    )]
                    #[serde(deny_unknown_fields)]
                    pub struct $provenance_root {
                        $(
                            $(#[$provenance_root_field_attr])*
                            $provenance_root_field:
                                $provenance_root_field_type,
                        )*
                    }
                ),
                "\n",
                $(
                    stringify!(
                        $(#[$provenance_struct_attr])*
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(deny_unknown_fields)]
                        $provenance_struct_visibility
                        struct $provenance_struct {
                            $(
                                $(#[$provenance_struct_field_attr])*
                                $provenance_struct_field:
                                    $provenance_struct_field_type,
                            )*
                        }
                    ),
                    "\n",
                )*
                $(
                    stringify!(
                        #[derive(
                            Clone,
                            Debug,
                            Deserialize,
                            Eq,
                            PartialEq,
                            Serialize
                        )]
                        #[serde(
                            deny_unknown_fields,
                            tag = "kind",
                            content = "value",
                            rename_all = "snake_case"
                        )]
                        enum $provenance_enum {
                            $(
                                #[serde(
                                    rename = $provenance_enum_wire
                                )]
                                $provenance_enum_variant {
                                    $(
                                        $provenance_enum_field:
                                            $provenance_enum_field_type,
                                    )*
                                },
                            )*
                        }
                    ),
                    "\n",
                )*
                stringify!($($declaration)*),
            );

        pub const CANONICAL_PROJECTION_ROUTES:
            &[CanonicalProjectionRoute] = &[
                $(
                    CanonicalProjectionRoute {
                        route_id: value_contract_route_id!(
                            $value_material,
                            $derived_field
                        ),
                        owner: value_contract_owner_descriptor!(
                            $derived_owner,
                            concat!(
                                stringify!($value_builder),
                                "::",
                                stringify!($derived_field),
                            ),
                            BuilderDerived,
                            concat!(
                                stringify!($value_material),
                                ".",
                                stringify!($derived_field),
                            ),
                            $derived_class,
                            $derived_order,
                            $derived_null_empty,
                            $derived_branch,
                            concat!(
                                stringify!($value_material),
                                ".",
                                stringify!($derived_field),
                            ),
                        ),
                        producer: CanonicalProjectionProducer::PublicBuilder {
                            function: stringify!($value_builder),
                        },
                        input_binding: value_contract_input_binding!(
                            $derived_input_binding,
                            $value_context,
                            $derived_field,
                        ),
                        assignment:
                            CanonicalProjectionAssignment::ValidatedContext {
                            source_context_owner: stringify!($value_context),
                            source_context_field: stringify!($derived_field),
                            target: value_contract_route_id!(
                                $value_material,
                                $derived_field
                            ),
                        },
                        disposition:
                            CanonicalMutationDisposition::$derived_disposition,
                        probe_memberships: &[
                            $(
                                CanonicalProjectionProbeMembership {
                                    probe: CanonicalPublicInputProbeId::new(
                                        CanonicalMutationCase::ValueContract,
                                        stringify!($value_material),
                                        stringify!($derived_probe),
                                    ),
                                    disposition:
                                        CanonicalProjectionProbeDisposition::
                                            $derived_probe_disposition,
                                },
                            )*
                        ],
                        mounts: &[
                            value_contract_route_mount!(
                                $derived_mount,
                                $derived_field
                            ),
                        ],
                        dependency_edges: &[
                            $(
                                CanonicalProjectionDependencyEdge {
                                    dependent_route:
                                        CanonicalProjectionRouteId {
                                            case: CanonicalMutationCase::
                                                $derived_dependency_case,
                                            material_owner: stringify!(
                                                $derived_dependency_owner
                                            ),
                                            material_field: stringify!(
                                                $derived_dependency_field
                                            ),
                                        },
                                },
                            )*
                        ],
                    },
                )*
                $(
                    typed_value_kind_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_unit_owner,
                        $typed_unit_normalized,
                        $typed_unit_variant,
                        $typed_unit_wire,
                        $typed_unit_mount,
                        probes [$($typed_unit_probe),+],
                    ),
                )*
                $(
                    typed_value_kind_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_direct_owner,
                        $typed_direct_normalized,
                        $typed_direct_variant,
                        $typed_direct_wire,
                        $typed_direct_kind_mount,
                        probes [$($typed_direct_kind_probe),+],
                    ),
                    typed_value_field_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_direct_value_owner,
                        $typed_direct_value_normalized,
                        $typed_direct_variant,
                        $typed_direct_wire,
                        $typed_direct_field,
                        "value",
                        $typed_direct_order,
                        $typed_direct_null_empty,
                        $typed_direct_branch,
                        $typed_direct_value_mount,
                        probes [$($typed_direct_value_probe),+],
                    ),
                )*
                $(
                    typed_value_kind_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_number_owner,
                        $typed_number_normalized,
                        $typed_number_variant,
                        $typed_number_wire,
                        $typed_number_kind_mount,
                        probes [$($typed_number_kind_probe),+],
                    ),
                    typed_value_field_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_number_value_owner,
                        $typed_number_value_normalized,
                        $typed_number_variant,
                        $typed_number_wire,
                        $typed_number_field,
                        "value",
                        $typed_number_order,
                        $typed_number_null_empty,
                        $typed_number_branch,
                        $typed_number_value_mount,
                        probes [$($typed_number_value_probe),+],
                    ),
                )*
                typed_value_kind_route!(
                    $typed_wrapper,
                    $typed_material,
                    $typed_input,
                    $typed_public_builder,
                    $typed_struct_owner,
                    $typed_struct_normalized,
                    $typed_struct_variant,
                    $typed_struct_wire,
                    $typed_struct_kind_mount,
                    probes [$($typed_struct_kind_probe),+],
                ),
                $(
                    typed_value_field_route!(
                        $typed_wrapper,
                        $typed_material,
                        $typed_input,
                        $typed_public_builder,
                        $typed_struct_field_owner,
                        $typed_struct_field_normalized,
                        $typed_struct_variant,
                        $typed_struct_wire,
                        $typed_struct_field,
                        $typed_struct_field_wire,
                        $typed_struct_order,
                        $typed_struct_null_empty,
                        $typed_struct_branch,
                        $typed_struct_field_mount,
                        probes [$($typed_struct_field_probe),+],
                    ),
                )*
                $(
                    source_field_route!(
                        $source_builder,
                        $source_root,
                        mounts $source_root_mounts,
                        $source_root_owner,
                        $source_root_path,
                        $source_root_order,
                        $source_root_null_empty,
                        $source_root_branch,
                        $source_root_member,
                        $source_root_field
                        $(, $source_root_disposition)?,
                    ),
                )*
                $(
                    $(
                        source_field_route!(
                            $source_builder,
                            $source_struct,
                            mounts $source_struct_mounts,
                            $source_struct_owner,
                            $source_struct_path,
                            $source_struct_order,
                            $source_struct_null_empty,
                            $source_struct_branch,
                            $source_struct_member,
                            $source_struct_field
                            $(, $source_struct_disposition)?,
                        ),
                    )*
                    $(
                        $(
                            source_singleton_route_from_variants!(
                                $source_builder,
                                $source_singleton_material,
                                mounts $source_struct_mounts,
                                $source_singleton_field,
                                $source_singleton_owner,
                                $source_singleton_path,
                                $source_singleton_order,
                                $source_singleton_null_empty,
                                $source_singleton_branch,
                                $source_singleton_member,
                                variants [
                                    $(
                                        $source_singleton_variant as
                                            $source_singleton_wire
                                    ),*
                                ]
                                $(, $source_singleton_disposition)?,
                            ),
                        )*
                    )?
                )*
                $(
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_unit_path,
                            $source_unit_owner,
                            $source_unit_order,
                            $source_unit_null_empty,
                            $source_unit_branch,
                            $source_unit_member,
                            $source_unit_variant,
                            $source_unit_wire
                            $(, $source_unit_disposition)?,
                        ),
                    )*
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_tuple_path,
                            $source_tuple_owner,
                            $source_tuple_order,
                            $source_tuple_null_empty,
                            $source_tuple_branch,
                            $source_tuple_member,
                            $source_tuple_variant,
                            $source_tuple_wire
                            $(, $source_tuple_disposition)?,
                        ),
                    )*
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_variant_path,
                            $source_variant_owner,
                            $source_variant_order,
                            $source_variant_null_empty,
                            $source_variant_branch,
                            $source_variant_member,
                            $source_variant,
                            $source_variant_wire
                            $(, $source_variant_disposition)?,
                        ),
                        $(
                            source_variant_field_route!(
                                $source_builder,
                                $source_enum,
                                mounts $source_enum_mounts,
                                $source_variant,
                                $source_variant_wire,
                                $source_enum_field_owner,
                                $source_enum_field_path,
                                $source_enum_field_order,
                                $source_enum_field_null_empty,
                                $source_enum_field_branch,
                                $source_enum_field_member,
                                $source_enum_field
                                $(, $source_enum_field_disposition)?,
                            ),
                        )*
                    )*
                )*
            ];

        pub const CANONICAL_SOURCE_LOADER_BRANCH_CANDIDATES:
            &[Option<&str>] = &[
                $(
                    $(
                        $(
                            $(
                                Some(concat!(
                                    stringify!($source_singleton_material),
                                    "::",
                                    stringify!($source_singleton_variant),
                                )),
                            )*
                        )*
                    )?
                )*
                $(
                    $(
                        source_loader_branch!(
                            concat!(
                                stringify!($source_enum),
                                "::",
                                stringify!($source_unit_variant),
                            )
                            $(, $source_unit_disposition)?,
                        ),
                    )*
                    $(
                        source_loader_branch!(
                            concat!(
                                stringify!($source_enum),
                                "::",
                                stringify!($source_tuple_variant),
                            )
                            $(, $source_tuple_disposition)?,
                        ),
                    )*
                    $(
                        source_loader_branch!(
                            concat!(
                                stringify!($source_enum),
                                "::",
                                stringify!($source_variant),
                            )
                            $(, $source_variant_disposition)?,
                        ),
                    )*
                )*
            ];

        pub const CANONICAL_SEMANTIC_DERIVED_INPUTS:
            &[(&str, &str)] = &[
                (
                    stringify!($semantic_root_builder),
                    stringify!($semantic_root_context),
                ),
                $(
                    $(
                        (
                            stringify!($semantic_struct_projector),
                            stringify!($semantic_struct_context),
                        ),
                    )?
                )*
                $(
                    $(
                        (
                            stringify!($semantic_enum_projector),
                            stringify!($semantic_enum_context),
                        ),
                    )?
                )*
            ];

        pub const CANONICAL_SEMANTIC_LOADER_BRANCH_CANDIDATES:
            &[&str] = &[
                $(
                    $(
                        concat!(
                            stringify!($semantic_enum),
                            "::",
                            stringify!($semantic_unit_variant),
                        ),
                    )*
                    $(
                        concat!(
                            stringify!($semantic_enum),
                            "::",
                            stringify!(
                                $semantic_composite_unit_variant
                            ),
                        ),
                    )*
                    $(
                        concat!(
                            stringify!($semantic_enum),
                            "::",
                            stringify!($semantic_tuple_variant),
                        ),
                    )*
                    $(
                        concat!(
                            stringify!($semantic_enum),
                            "::",
                            stringify!(
                                $semantic_tuple_struct_variant
                            ),
                        ),
                    )*
                    $(
                        concat!(
                            stringify!($semantic_enum),
                            "::",
                            stringify!($semantic_variant),
                        ),
                    )*
                )*
                $(
                    concat!(
                        stringify!($semantic_singleton),
                        "::",
                        stringify!($semantic_singleton_variant),
                    ),
                )*
            ];

        pub const CANONICAL_PROVENANCE_DERIVED_INPUTS:
            &[(&str, &str)] = &[
                $(
                    (
                        stringify!($provenance_builder),
                        stringify!($provenance_parameter),
                    ),
                )*
                $(
                    (
                        stringify!($provenance_context_validator),
                        stringify!($provenance_context_field),
                    ),
                )*
            ];

        pub const CANONICAL_PROVENANCE_LOADER_BRANCH_CANDIDATES:
            &[&str] = &[
                $(
                    $(
                        concat!(
                            stringify!($provenance_enum),
                            "::",
                            stringify!($provenance_enum_variant),
                        ),
                    )*
                )*
            ];

        pub const CANONICAL_PROVENANCE_DERIVED_DEPENDENCIES:
            &[CanonicalDerivedDependencyDescriptor] = &[
                $(
                    CanonicalDerivedDependencyDescriptor {
                        changed_input: $provenance_dependency_input,
                        material_member: $provenance_dependency_member,
                        rule: $provenance_dependency_rule,
                    },
                )*
            ];

        pub const CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS:
            &[CanonicalOwnerPathDescriptor] = &[
                $(
                    CanonicalOwnerPathDescriptor {
                        normalized_owner: $owner,
                        normalized_member: $normalized_member,
                        normalized_source:
                            CanonicalDeclarationSource::$normalized_source,
                        domain: $domain,
                        canonical_path: $path,
                        owner_class: $class,
                        order: $order,
                        null_empty: $null_empty,
                        branch_type: $branch,
                        material_member: $member,
                        material_source:
                            CanonicalDeclarationSource::$material_source,
                    },
                )*
                $(
                    value_contract_owner_descriptor!(
                        $value_owner,
                        $value_normalized_member,
                        $value_normalized_source,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($value_field),
                        ),
                        $value_class,
                        $value_order,
                        $value_null_empty,
                        $value_branch,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($value_field),
                        ),
                    ),
                )*
                $(
                    value_contract_owner_descriptor!(
                        $derived_owner,
                        concat!(
                            stringify!($value_builder),
                            "::",
                            stringify!($derived_field),
                        ),
                        BuilderDerived,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($derived_field),
                        ),
                        $derived_class,
                        $derived_order,
                        $derived_null_empty,
                        $derived_branch,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($derived_field),
                        ),
                    ),
                )*
                $(
                    $(
                        value_contract_owner_descriptor!(
                            $struct_owner,
                            $struct_normalized_member,
                            ValueContract,
                            concat!(
                                stringify!($struct_material),
                                ".",
                                stringify!($struct_field),
                            ),
                            $struct_class,
                            $struct_order,
                            $struct_null_empty,
                            $struct_branch,
                            concat!(
                                stringify!($struct_material),
                                ".",
                                stringify!($struct_field),
                            ),
                        ),
                    )*
                )*
                $(
                    $(
                        value_contract_owner_descriptor!(
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($unit_variant),
                            ),
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($unit_variant),
                            ),
                            ValueContract,
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $unit_wire,
                                "}.kind",
                            ),
                            "normalized",
                            "scalar",
                            "required",
                            $unit_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($unit_variant),
                            ),
                        ),
                    )*
                    $(
                        value_contract_owner_descriptor!(
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($tuple_variant),
                            ),
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($tuple_variant),
                            ),
                            ValueContract,
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $tuple_wire,
                                "}.kind",
                            ),
                            "normalized",
                            "scalar",
                            "required",
                            $tuple_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($tuple_variant),
                            ),
                        ),
                        value_contract_owner_descriptor!(
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($tuple_variant),
                                ".",
                                stringify!($tuple_field),
                            ),
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($tuple_variant),
                                ".",
                                stringify!($tuple_field),
                            ),
                            ValueContract,
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $tuple_wire,
                                "}.value",
                            ),
                            "normalized",
                            $tuple_order,
                            $tuple_null_empty,
                            $tuple_branch,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($tuple_variant),
                                ".value",
                            ),
                        ),
                    )*
                    $(
                        value_contract_owner_descriptor!(
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($enum_variant),
                            ),
                            concat!(
                                stringify!($enum_input),
                                "::",
                                stringify!($enum_variant),
                            ),
                            ValueContract,
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $enum_wire,
                                "}.kind",
                            ),
                            "normalized",
                            "scalar",
                            "required",
                            $enum_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($enum_variant),
                            ),
                        ),
                        $(
                            value_contract_owner_descriptor!(
                                concat!(
                                    stringify!($enum_input),
                                    "::",
                                    stringify!($enum_variant),
                                    ".",
                                    stringify!($enum_field),
                                ),
                                concat!(
                                    stringify!($enum_input),
                                    "::",
                                    stringify!($enum_variant),
                                    ".",
                                    stringify!($enum_field),
                                ),
                                ValueContract,
                                concat!(
                                    stringify!($enum_wrapper),
                                    "{kind=",
                                    $enum_wire,
                                    "}.value.",
                                    stringify!($enum_field),
                                ),
                                "normalized",
                                $enum_order,
                                $enum_null_empty,
                                $enum_branch,
                                concat!(
                                    stringify!($enum_material),
                                    "::",
                                    stringify!($enum_variant),
                                    ".",
                                    stringify!($enum_field),
                                ),
                            ),
                        )*
                    )*
                )*
                $(
                    value_contract_owner_descriptor!(
                        $typed_unit_owner,
                        $typed_unit_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_unit_wire,
                            "}.kind",
                        ),
                        "normalized",
                        "scalar",
                        "required",
                        $typed_unit_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_unit_variant),
                        ),
                    ),
                )*
                $(
                    value_contract_owner_descriptor!(
                        $typed_direct_owner,
                        $typed_direct_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_direct_wire,
                            "}.kind",
                        ),
                        "normalized",
                        "scalar",
                        "required",
                        $typed_direct_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_direct_variant),
                        ),
                    ),
                    value_contract_owner_descriptor!(
                        $typed_direct_value_owner,
                        $typed_direct_value_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_direct_wire,
                            "}.value",
                        ),
                        "normalized",
                        $typed_direct_order,
                        $typed_direct_null_empty,
                        $typed_direct_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_direct_variant),
                            ".value",
                        ),
                    ),
                )*
                $(
                    value_contract_owner_descriptor!(
                        $typed_number_owner,
                        $typed_number_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_number_wire,
                            "}.kind",
                        ),
                        "normalized",
                        "scalar",
                        "required",
                        $typed_number_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_number_variant),
                        ),
                    ),
                    value_contract_owner_descriptor!(
                        $typed_number_value_owner,
                        $typed_number_value_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_number_wire,
                            "}.value",
                        ),
                        "normalized",
                        $typed_number_order,
                        $typed_number_null_empty,
                        $typed_number_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_number_variant),
                            ".value",
                        ),
                    ),
                )*
                value_contract_owner_descriptor!(
                    $typed_struct_owner,
                    $typed_struct_normalized,
                    ValueContract,
                    concat!(
                        stringify!($typed_wrapper),
                        "{kind=",
                        $typed_struct_wire,
                        "}.kind",
                    ),
                    "normalized",
                    "scalar",
                    "required",
                    $typed_struct_wire,
                    concat!(
                        stringify!($typed_material),
                        "::",
                        stringify!($typed_struct_variant),
                    ),
                ),
                $(
                    value_contract_owner_descriptor!(
                        $typed_struct_field_owner,
                        $typed_struct_field_normalized,
                        ValueContract,
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_struct_wire,
                            "}.value.",
                            $typed_struct_field_wire,
                        ),
                        "normalized",
                        $typed_struct_order,
                        $typed_struct_null_empty,
                        $typed_struct_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_struct_variant),
                            ".",
                            stringify!($typed_struct_field),
                        ),
                    ),
                )*
                $(
                    source_field_route!(
                        $source_builder,
                        $source_root,
                        mounts $source_root_mounts,
                        $source_root_owner,
                        $source_root_path,
                        $source_root_order,
                        $source_root_null_empty,
                        $source_root_branch,
                        $source_root_member,
                        $source_root_field
                        $(, $source_root_disposition)?,
                    )
                    .owner,
                )*
                $(
                    $(
                        source_field_route!(
                            $source_builder,
                            $source_struct,
                            mounts $source_struct_mounts,
                            $source_struct_owner,
                            $source_struct_path,
                            $source_struct_order,
                            $source_struct_null_empty,
                            $source_struct_branch,
                            $source_struct_member,
                            $source_struct_field
                            $(, $source_struct_disposition)?,
                        )
                        .owner,
                    )*
                    $(
                        $(
                            source_singleton_route_from_variants!(
                                $source_builder,
                                $source_singleton_material,
                                mounts $source_struct_mounts,
                                $source_singleton_field,
                                $source_singleton_owner,
                                $source_singleton_path,
                                $source_singleton_order,
                                $source_singleton_null_empty,
                                $source_singleton_branch,
                                $source_singleton_member,
                                variants [
                                    $(
                                        $source_singleton_variant as
                                            $source_singleton_wire
                                    ),*
                                ]
                                $(, $source_singleton_disposition)?,
                            )
                            .owner,
                        )*
                    )?
                )*
                $(
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_unit_path,
                            $source_unit_owner,
                            $source_unit_order,
                            $source_unit_null_empty,
                            $source_unit_branch,
                            $source_unit_member,
                            $source_unit_variant,
                            $source_unit_wire
                            $(, $source_unit_disposition)?,
                        )
                        .owner,
                    )*
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_tuple_path,
                            $source_tuple_owner,
                            $source_tuple_order,
                            $source_tuple_null_empty,
                            $source_tuple_branch,
                            $source_tuple_member,
                            $source_tuple_variant,
                            $source_tuple_wire
                            $(, $source_tuple_disposition)?,
                        )
                        .owner,
                    )*
                    $(
                        source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_variant_path,
                            $source_variant_owner,
                            $source_variant_order,
                            $source_variant_null_empty,
                            $source_variant_branch,
                            $source_variant_member,
                            $source_variant,
                            $source_variant_wire
                            $(, $source_variant_disposition)?,
                        )
                        .owner,
                        $(
                            source_variant_field_route!(
                                $source_builder,
                                $source_enum,
                                mounts $source_enum_mounts,
                                $source_variant,
                                $source_variant_wire,
                                $source_enum_field_owner,
                                $source_enum_field_path,
                                $source_enum_field_order,
                                $source_enum_field_null_empty,
                                $source_enum_field_branch,
                                $source_enum_field_member,
                                $source_enum_field
                                $(, $source_enum_field_disposition)?,
                            )
                            .owner,
                        )*
                    )*
                )*
                semantic_owner_descriptor!(
                    $semantic_derived_owner,
                    $semantic_derived_normalized_member,
                    $semantic_derived_source,
                    $semantic_derived_path,
                    $semantic_derived_class,
                    $semantic_derived_order,
                    $semantic_derived_null_empty,
                    $semantic_derived_branch,
                    $semantic_derived_member,
                ),
                $(
                    semantic_owner_descriptor!(
                        $semantic_root_owner,
                        $semantic_root_normalized_member,
                        $semantic_root_normalized_source,
                        $semantic_root_path,
                        $semantic_root_class,
                        $semantic_root_order,
                        $semantic_root_null_empty,
                        $semantic_root_branch,
                        $semantic_root_member,
                    ),
                )*
                $(
                    $(
                        semantic_owner_descriptor!(
                            $semantic_struct_owner,
                            $semantic_struct_normalized_member,
                            $semantic_struct_normalized_source,
                            $semantic_struct_path,
                            $semantic_struct_class,
                            $semantic_struct_order,
                            $semantic_struct_null_empty,
                            $semantic_struct_branch,
                            $semantic_struct_member,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_owner_descriptor!(
                            $semantic_generic_owner,
                            $semantic_generic_normalized_member,
                            $semantic_generic_normalized_source,
                            $semantic_generic_path,
                            $semantic_generic_class,
                            $semantic_generic_order,
                            $semantic_generic_null_empty,
                            $semantic_generic_branch,
                            $semantic_generic_member,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_owner_descriptor!(
                            $semantic_closed_owner,
                            $semantic_closed_normalized_member,
                            $semantic_closed_normalized_source,
                            $semantic_closed_path,
                            $semantic_closed_class,
                            $semantic_closed_order,
                            $semantic_closed_null_empty,
                            $semantic_closed_branch,
                            $semantic_closed_member,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_owner_descriptor!(
                            $semantic_unit_owner,
                            $semantic_unit_normalized_member,
                            $semantic_unit_normalized_source,
                            $semantic_unit_path,
                            $semantic_unit_class,
                            $semantic_unit_order,
                            $semantic_unit_null_empty,
                            $semantic_unit_branch,
                            $semantic_unit_member,
                        ),
                    )*
                    $(
                        semantic_owner_descriptor!(
                            $semantic_tuple_owner,
                            $semantic_tuple_normalized_member,
                            $semantic_tuple_normalized_source,
                            $semantic_tuple_path,
                            $semantic_tuple_class,
                            $semantic_tuple_order,
                            $semantic_tuple_null_empty,
                            $semantic_tuple_branch,
                            $semantic_tuple_member,
                        ),
                    )*
                    $(
                        semantic_owner_descriptor!(
                            $semantic_tuple_struct_owner,
                            $semantic_tuple_struct_normalized_member,
                            $semantic_tuple_struct_normalized_source,
                            $semantic_tuple_struct_path,
                            $semantic_tuple_struct_class,
                            $semantic_tuple_struct_order,
                            $semantic_tuple_struct_null_empty,
                            $semantic_tuple_struct_branch,
                            $semantic_tuple_struct_member,
                        ),
                        $(
                            semantic_owner_descriptor!(
                                $semantic_tuple_struct_field_owner,
                                $semantic_tuple_struct_field_normalized_member,
                                $semantic_tuple_struct_field_normalized_source,
                                $semantic_tuple_struct_field_path,
                                $semantic_tuple_struct_field_class,
                                $semantic_tuple_struct_field_order,
                                $semantic_tuple_struct_field_null_empty,
                                $semantic_tuple_struct_field_branch,
                                $semantic_tuple_struct_field_member,
                            ),
                        )*
                    )*
                    $(
                        semantic_owner_descriptor!(
                            $semantic_variant_owner,
                            $semantic_variant_normalized_member,
                            $semantic_variant_normalized_source,
                            $semantic_variant_path,
                            $semantic_variant_class,
                            $semantic_variant_order,
                            $semantic_variant_null_empty,
                            $semantic_variant_branch,
                            $semantic_variant_member,
                        ),
                        $(
                            semantic_owner_descriptor!(
                                $semantic_enum_field_owner,
                                $semantic_enum_field_normalized_member,
                                $semantic_enum_field_normalized_source,
                                $semantic_enum_field_path,
                                $semantic_enum_field_class,
                                $semantic_enum_field_order,
                                $semantic_enum_field_null_empty,
                                $semantic_enum_field_branch,
                                $semantic_enum_field_member,
                            ),
                        )*
                    )*
                )*
                $(
                    CanonicalOwnerPathDescriptor {
                        normalized_owner: $provenance_owner,
                        normalized_member:
                            $provenance_normalized_member,
                        normalized_source:
                            CanonicalDeclarationSource::
                                $provenance_normalized_source,
                        domain: $provenance_domain,
                        canonical_path: $provenance_path,
                        owner_class: $provenance_class,
                        order: $provenance_order,
                        null_empty: $provenance_null_empty,
                        branch_type: $provenance_branch,
                        material_member: $provenance_member,
                        material_source:
                            CanonicalDeclarationSource::
                                $provenance_material_source,
                    },
                )*
            ];

        pub const CANONICAL_PROJECTION_MUTATION_DESCRIPTORS:
            &[CanonicalMutationDescriptor] = &[
                $(
                    CanonicalMutationDescriptor {
                        case: CanonicalMutationCase::$case,
                        disposition: CanonicalMutationDisposition::$disposition,
                        material_source:
                            CanonicalDeclarationSource::$material_source,
                        domain: $domain,
                        canonical_path: $path,
                        material_member: $member,
                        branch_type: $branch,
                        null_empty: $null_empty,
                    },
                )*
                $(
                    value_contract_mutation_descriptor!(
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($value_field),
                        ),
                        $value_null_empty,
                        $value_branch,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($value_field),
                        ),
                    ),
                )*
                $(
                    value_contract_mutation_descriptor!(
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($derived_field),
                        ),
                        $derived_null_empty,
                        $derived_branch,
                        concat!(
                            stringify!($value_material),
                            ".",
                            stringify!($derived_field),
                        ),
                        $derived_disposition,
                    ),
                )*
                $(
                    $(
                        value_contract_mutation_descriptor!(
                            concat!(
                                stringify!($struct_material),
                                ".",
                                stringify!($struct_field),
                            ),
                            $struct_null_empty,
                            $struct_branch,
                            concat!(
                                stringify!($struct_material),
                                ".",
                                stringify!($struct_field),
                            ),
                        ),
                    )*
                )*
                $(
                    $(
                        value_contract_mutation_descriptor!(
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $unit_wire,
                                "}.kind",
                            ),
                            "required",
                            $unit_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($unit_variant),
                            ),
                        ),
                    )*
                    $(
                        value_contract_mutation_descriptor!(
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $tuple_wire,
                                "}.kind",
                            ),
                            "required",
                            $tuple_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($tuple_variant),
                            ),
                        ),
                        value_contract_mutation_descriptor!(
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $tuple_wire,
                                "}.value",
                            ),
                            $tuple_null_empty,
                            $tuple_branch,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($tuple_variant),
                                ".value",
                            ),
                        ),
                    )*
                    $(
                        value_contract_mutation_descriptor!(
                            concat!(
                                stringify!($enum_wrapper),
                                "{kind=",
                                $enum_wire,
                                "}.kind",
                            ),
                            "required",
                            $enum_wire,
                            concat!(
                                stringify!($enum_material),
                                "::",
                                stringify!($enum_variant),
                            ),
                        ),
                        $(
                            value_contract_mutation_descriptor!(
                                concat!(
                                    stringify!($enum_wrapper),
                                    "{kind=",
                                    $enum_wire,
                                    "}.value.",
                                    stringify!($enum_field),
                                ),
                                $enum_null_empty,
                                $enum_branch,
                                concat!(
                                    stringify!($enum_material),
                                    "::",
                                    stringify!($enum_variant),
                                    ".",
                                    stringify!($enum_field),
                                ),
                            ),
                        )*
                    )*
                )*
                $(
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_unit_wire,
                            "}.kind",
                        ),
                        "required",
                        $typed_unit_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_unit_variant),
                        ),
                    ),
                )*
                $(
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_direct_wire,
                            "}.kind",
                        ),
                        "required",
                        $typed_direct_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_direct_variant),
                        ),
                    ),
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_direct_wire,
                            "}.value",
                        ),
                        $typed_direct_null_empty,
                        $typed_direct_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_direct_variant),
                            ".value",
                        ),
                    ),
                )*
                $(
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_number_wire,
                            "}.kind",
                        ),
                        "required",
                        $typed_number_wire,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_number_variant),
                        ),
                    ),
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_number_wire,
                            "}.value",
                        ),
                        $typed_number_null_empty,
                        $typed_number_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_number_variant),
                            ".value",
                        ),
                    ),
                )*
                typed_value_mutation_descriptor!(
                    concat!(
                        stringify!($typed_wrapper),
                        "{kind=",
                        $typed_struct_wire,
                        "}.kind",
                    ),
                    "required",
                    $typed_struct_wire,
                    concat!(
                        stringify!($typed_material),
                        "::",
                        stringify!($typed_struct_variant),
                    ),
                ),
                $(
                    typed_value_mutation_descriptor!(
                        concat!(
                            stringify!($typed_wrapper),
                            "{kind=",
                            $typed_struct_wire,
                            "}.value.",
                            $typed_struct_field_wire,
                        ),
                        $typed_struct_null_empty,
                        $typed_struct_branch,
                        concat!(
                            stringify!($typed_material),
                            "::",
                            stringify!($typed_struct_variant),
                            ".",
                            stringify!($typed_struct_field),
                        ),
                    ),
                )*
                $(
                    source_mutation_view(source_field_route!(
                        $source_builder,
                        $source_root,
                        mounts $source_root_mounts,
                        $source_root_owner,
                        $source_root_path,
                        $source_root_order,
                        $source_root_null_empty,
                        $source_root_branch,
                        $source_root_member,
                        $source_root_field
                        $(, $source_root_disposition)?,
                    )),
                )*
                $(
                    $(
                        source_mutation_view(source_field_route!(
                            $source_builder,
                            $source_struct,
                            mounts $source_struct_mounts,
                            $source_struct_owner,
                            $source_struct_path,
                            $source_struct_order,
                            $source_struct_null_empty,
                            $source_struct_branch,
                            $source_struct_member,
                            $source_struct_field
                            $(, $source_struct_disposition)?,
                        )),
                    )*
                    $(
                        $(
                            source_mutation_view(
                                source_singleton_route_from_variants!(
                                    $source_builder,
                                    $source_singleton_material,
                                    mounts $source_struct_mounts,
                                    $source_singleton_field,
                                    $source_singleton_owner,
                                    $source_singleton_path,
                                    $source_singleton_order,
                                    $source_singleton_null_empty,
                                    $source_singleton_branch,
                                    $source_singleton_member,
                                    variants [
                                        $(
                                            $source_singleton_variant as
                                                $source_singleton_wire
                                        ),*
                                    ]
                                    $(, $source_singleton_disposition)?,
                                ),
                            ),
                        )*
                    )?
                )*
                $(
                    $(
                        source_mutation_view(source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_unit_path,
                            $source_unit_owner,
                            $source_unit_order,
                            $source_unit_null_empty,
                            $source_unit_branch,
                            $source_unit_member,
                            $source_unit_variant,
                            $source_unit_wire
                            $(, $source_unit_disposition)?,
                        )),
                    )*
                    $(
                        source_mutation_view(source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_tuple_path,
                            $source_tuple_owner,
                            $source_tuple_order,
                            $source_tuple_null_empty,
                            $source_tuple_branch,
                            $source_tuple_member,
                            $source_tuple_variant,
                            $source_tuple_wire
                            $(, $source_tuple_disposition)?,
                        )),
                    )*
                    $(
                        source_mutation_view(source_variant_route!(
                            $source_builder,
                            $source_enum,
                            mounts $source_enum_mounts,
                            $source_variant_path,
                            $source_variant_owner,
                            $source_variant_order,
                            $source_variant_null_empty,
                            $source_variant_branch,
                            $source_variant_member,
                            $source_variant,
                            $source_variant_wire
                            $(, $source_variant_disposition)?,
                        )),
                        $(
                            source_mutation_view(source_variant_field_route!(
                                $source_builder,
                                $source_enum,
                                mounts $source_enum_mounts,
                                $source_variant,
                                $source_variant_wire,
                                $source_enum_field_owner,
                                $source_enum_field_path,
                                $source_enum_field_order,
                                $source_enum_field_null_empty,
                                $source_enum_field_branch,
                                $source_enum_field_member,
                                $source_enum_field
                                $(, $source_enum_field_disposition)?,
                            )),
                        )*
                    )*
                )*
                semantic_mutation_descriptor!(
                    $semantic_derived_path,
                    $semantic_derived_null_empty,
                    $semantic_derived_branch,
                    $semantic_derived_member,
                ),
                $(
                    semantic_mutation_descriptor!(
                        $semantic_root_path,
                        $semantic_root_null_empty,
                        $semantic_root_branch,
                        $semantic_root_member,
                    ),
                )*
                $(
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_struct_path,
                            $semantic_struct_null_empty,
                            $semantic_struct_branch,
                            $semantic_struct_member
                            $(, $semantic_struct_disposition)?,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_generic_path,
                            $semantic_generic_null_empty,
                            $semantic_generic_branch,
                            $semantic_generic_member,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_closed_path,
                            $semantic_closed_null_empty,
                            $semantic_closed_branch,
                            $semantic_closed_member,
                        ),
                    )*
                )*
                $(
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_unit_path,
                            $semantic_unit_null_empty,
                            $semantic_unit_branch,
                            $semantic_unit_member,
                        ),
                    )*
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_tuple_path,
                            $semantic_tuple_null_empty,
                            $semantic_tuple_branch,
                            $semantic_tuple_member,
                        ),
                    )*
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_tuple_struct_path,
                            $semantic_tuple_struct_null_empty,
                            $semantic_tuple_struct_branch,
                            $semantic_tuple_struct_member,
                        ),
                        $(
                            semantic_mutation_descriptor!(
                                $semantic_tuple_struct_field_path,
                                $semantic_tuple_struct_field_null_empty,
                                $semantic_tuple_struct_field_branch,
                                $semantic_tuple_struct_field_member,
                            ),
                        )*
                    )*
                    $(
                        semantic_mutation_descriptor!(
                            $semantic_variant_path,
                            $semantic_variant_null_empty,
                            $semantic_variant_branch,
                            $semantic_variant_member,
                        ),
                        $(
                            semantic_mutation_descriptor!(
                                $semantic_enum_field_path,
                                $semantic_enum_field_null_empty,
                                $semantic_enum_field_branch,
                                $semantic_enum_field_member,
                            ),
                        )*
                    )*
                )*
                $(
                    CanonicalMutationDescriptor {
                        case:
                            CanonicalMutationCase::$provenance_case,
                        disposition:
                            CanonicalMutationDisposition::
                                $provenance_disposition,
                        material_source:
                            CanonicalDeclarationSource::
                                $provenance_material_source,
                        domain: $provenance_domain,
                        canonical_path: $provenance_path,
                        material_member: $provenance_member,
                        branch_type: $provenance_branch,
                        null_empty: $provenance_null_empty,
                    },
                )*
            ];
    };
}

projection_schema! {
owner_paths {
    (
        "StableSemver.major",
        "StableSemver.major",
        Model,
        "semantic",
        "StableSemver.major",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.major",
        Model,
        Semantic,
        Mutable,
    );

    (
        "StableSemver.minor",
        "StableSemver.minor",
        Model,
        "semantic",
        "StableSemver.minor",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.minor",
        Model,
        Semantic,
        Mutable,
    );

    (
        "StableSemver.patch",
        "StableSemver.patch",
        Model,
        "semantic",
        "StableSemver.patch",
        "normalized",
        "scalar",
        "required",
        "u32",
        "StableSemver.patch",
        Model,
        Semantic,
        Mutable,
    );


}
value_contract_projection {
    root ValueContractMaterialV1 via ValueContractProjectionContext
        using value_contract_material {
        catalog_fields {
            (
                "ValueContractCatalog.named_objects",
                "ValueContractCatalog.named_objects",
                ValueContract,
                "normalized",
                "utf16_member_name",
                "empty_object",
                "Map<string,NamedObjectMaterialV1>",
                named_objects: BTreeMap<String, NamedObjectMaterialV1> =
                    validated_map(named_object_material),
            );
            (
                "ValueContractCatalog.roots",
                "ValueContractCatalog.roots",
                ValueContract,
                "normalized",
                "utf16_member_name",
                "empty_object",
                "Map<string,FieldMaterialV1>",
                roots: BTreeMap<String, FieldMaterialV1> = map(field_material),
            );
        }
        derived_fields {
            (
                "ValueContractCatalog.value_language_epoch",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                value_language_epoch: u32 = nonzero_epoch,
                route {
                    disposition Mutable;
                    input PublicParameter;
                    mount RootField;
                    probes {
                        ValueContractEpoch => Accepted;
                    }
                    dependencies {
                    }
                },
            );
        }
    }
    structs {
        struct NamedObjectMaterialV1 from ValueObjectContract
            using named_object_material {
            (
                "NamedObject.fields",
                "ValueObjectContract.fields",
                "normalized",
                "utf16_member_name",
                "empty_object",
                "Map<string,FieldMaterialV1>",
                fields: BTreeMap<String, FieldMaterialV1> =
                    validated_map(field_material),
            );
        }
        struct FieldMaterialV1 from ValueContractField using field_material {
            (
                "Field.required",
                "ValueContractField.required",
                "normalized",
                "scalar",
                "required",
                "bool",
                required: bool = copy,
            );
            (
                "Field.type_ref",
                "ValueContractField.type_ref",
                "normalized",
                "scalar",
                "required",
                "TypeRefMaterialV1",
                type_ref: TypeRefMaterialV1 = project(type_ref_material),
            );
        }
        struct TypeRefMaterialV1 from TypeRef using type_ref_material {
            (
                "TypeRef.nullable",
                "TypeRef.nullable",
                "normalized",
                "scalar",
                "required",
                "bool",
                nullable: bool = copy,
            );
            (
                "TypeRef.value_type",
                "TypeRef.value_type",
                "normalized",
                "scalar",
                "required",
                "ValueTypeMaterialV1",
                value_type: ValueTypeMaterialV1 =
                    project(value_type_material),
            );
        }
    }
    transparent_enums {
        transparent ValueTypeMaterialV1 wraps ValueTypeMaterial
            from ValueType using value_type_material {
            unit_variants {}
            tuple_from_struct_variants {
                (
                    Scalar as "scalar",
                    scalar: ValueScalarMaterialV1 =
                        project(scalar_material),
                    "scalar",
                    "required",
                    "ValueScalarMaterialV1",
                );
            }
            struct_variants {
                (
                    Enum as "enum" {
                        (
                            name: String = validated_clone,
                            "scalar",
                            "required",
                            "string",
                        );
                        (
                            values: Vec<String> = validated_list_clone,
                            "declared",
                            "empty_array",
                            "Vec<string>",
                        );
                    }
                );
                (
                    Object as "object" {
                        (
                            fields: BTreeMap<String, FieldMaterialV1> =
                                validated_map(field_material),
                            "utf16_member_name",
                            "empty_object",
                            "Map<string,FieldMaterialV1>",
                        );
                    }
                );
                (
                    List as "list" {
                        (
                            element: Box<TypeRefMaterialV1> =
                                boxed_project(type_ref_material),
                            "scalar",
                            "required",
                            "TypeRefMaterialV1",
                        );
                    }
                );
                (
                    Ref as "ref" {
                        (
                            name: String = validated_clone,
                            "scalar",
                            "required",
                            "string",
                        );
                    }
                );
            }
        }
        transparent ValueScalarMaterialV1 wraps ValueScalarMaterial
            from ValueScalar using scalar_material {
            unit_variants {
                Boolean as "boolean";
                String as "string";
                Int32 as "int32";
                Int64 as "int64";
                UInt64 as "uint64";
                Decimal as "decimal";
                Uuid as "uuid";
                Date as "date";
                Timestamp as "timestamp";
                TimestampTz as "timestamptz";
                Json as "json";
            }
            tuple_from_struct_variants {}
            struct_variants {
                (
                    Custom as "custom" {
                        (
                            name: String = validated_clone,
                            "scalar",
                            "required",
                            "string",
                        );
                    }
                );
            }
        }
    }
}
typed_value_projection {
    transparent TypedValueMaterialV1 wraps TypedValueMaterial
        from TypedValue using from_typed_value
        exposed_as typed_value_material {
        unit_variants {
            (
                "TypedValue::Null",
                "TypedValue::Null",
                Null as "null",
                route {
                    probes {
                        NullKind;
                    }
                    mount TaggedKind;
                },
            );
        }
        direct_tuple_variants {
            (
                "TypedValue::Boolean",
                "TypedValue::Boolean",
                Boolean as "boolean",
                "TypedValue::Boolean.value",
                "TypedValue::Boolean.value",
                value: bool = copy,
                "scalar",
                "required",
                "bool",
                routes {
                    kind {
                        probes {
                            BooleanKind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            BooleanKind;
                            BooleanValue;
                            NullKind;
                        }
                        mount TaggedValue;
                    }
                },
            );
            (
                "TypedValue::String",
                "TypedValue::String",
                String as "string",
                "TypedValue::String.value",
                "TypedValue::String.value",
                value: String = clone,
                "scalar",
                "required",
                "string",
                routes {
                    kind {
                        probes {
                            StringKind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            StringKind;
                            StringValue;
                        }
                        mount TaggedValue;
                    }
                },
            );
            (
                "TypedValue::List",
                "TypedValue::List",
                List as "list",
                "TypedValue::List.value",
                "TypedValue::List.value",
                value: Vec<TypedValueMaterial> = recursive_list(project),
                "declared",
                "empty_array",
                "Vec<TypedValueMaterialV1>",
                routes {
                    kind {
                        probes {
                            ListKind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            ListKind;
                            ListValue;
                        }
                        mount TaggedValue;
                    }
                },
            );
            (
                "TypedValue::Object",
                "TypedValue::Object",
                Object as "object",
                "TypedValue::Object.value",
                "TypedValue::Object.value",
                value: BTreeMap<String, TypedValueMaterial> =
                    recursive_map(project),
                "utf16_member_name",
                "empty_object",
                "Map<string,TypedValueMaterialV1>",
                routes {
                    kind {
                        probes {
                            ObjectKind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            ObjectKind;
                        }
                        mount TaggedValue;
                    }
                },
            );
        }
        number_tuple_variants {
            (
                "TypedValue::I64",
                "CanonicalNumber::I64",
                I64 as "i64",
                "TypedValue::I64.value",
                "CanonicalNumber::I64.value",
                value: String = integer_string,
                "scalar",
                "required",
                "i64-string",
                routes {
                    kind {
                        probes {
                            I64Kind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            I64Kind;
                            I64Value;
                        }
                        mount TaggedValue;
                    }
                },
            );
            (
                "TypedValue::U64",
                "CanonicalNumber::U64",
                U64 as "u64",
                "TypedValue::U64.value",
                "CanonicalNumber::U64.value",
                value: String = integer_string,
                "scalar",
                "required",
                "u64-string",
                routes {
                    kind {
                        probes {
                            U64Kind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            U64Kind;
                            U64Value;
                        }
                        mount TaggedValue;
                    }
                },
            );
            (
                "TypedValue::Decimal",
                "CanonicalNumber::Decimal",
                Decimal as "decimal",
                "TypedValue::Decimal.value",
                "CanonicalNumber::Decimal.value",
                value: String = decimal_string,
                "scalar",
                "required",
                "decimal-string",
                routes {
                    kind {
                        probes {
                            DecimalKind;
                        }
                        mount TaggedKind;
                    }
                    value {
                        probes {
                            DecimalKind;
                            DecimalValue;
                        }
                        mount TaggedValue;
                    }
                },
            );
        }
        struct_variant {
            (
                "TypedValue::InlineBytes",
                "TypedValue::InlineBytes",
                InlineBytes as "inline_bytes",
                value {
                    (
                        "TypedValue::InlineBytes.bytes",
                        "BoundedInlineBytes.bytes",
                        "$binary",
                        binary: String = base64url,
                        "scalar",
                        "required",
                        "base64url",
                        route {
                            probes {
                                InlineBytesKind;
                                InlineBytesBinary;
                            }
                            mount TaggedStructField;
                        },
                    );
                    (
                        "TypedValue::InlineBytes.file_name",
                        "BoundedInlineBytes.file_name",
                        "file_name",
                        file_name: Option<String> = file_name,
                        "scalar",
                        "explicit_null",
                        "Option<string>",
                        route {
                            probes {
                                InlineBytesKind;
                                InlineBytesFileName;
                            }
                            mount TaggedStructField;
                        },
                    );
                    (
                        "TypedValue::InlineBytes.media_type",
                        "BoundedInlineBytes.media_type",
                        "media_type",
                        media_type: Option<String> = media_type,
                        "scalar",
                        "explicit_null",
                        "Option<string>",
                        route {
                            probes {
                                InlineBytesKind;
                                InlineBytesMediaType;
                            }
                            mount TaggedStructField;
                        },
                    );
                },
                kind_route {
                    probes {
                        InlineBytesKind;
                    }
                    mount TaggedKind;
                },
            );
        }
    }
}
source_record_projection {
    root SourceRecordMaterialV1 from (ConnectorSourceRecord)
        using source_record_material
        mounts {
            [];
        }
    {
        (
            "ConnectorSourceRecord.record_version",
            "SourceRecordMaterialV1.record_version",
            "scalar",
            "required",
            "Epoch",
            "SourceRecordMaterialV1.record_version",
            record_version: u32 = member { copy },
        );
        (
            "ConnectorSourceRecord.record_id",
            "SourceRecordMaterialV1.record_id",
            "scalar",
            "required",
            "SourceRecordId",
            "SourceRecordMaterialV1.record_id",
            record_id: String = member { as_str_owned },
        );
        (
            "ConnectorSourceRecord.subject",
            "SourceRecordMaterialV1.subject",
            "scalar",
            "required",
            "SourceSubjectMaterialV1",
            "SourceRecordMaterialV1.subject",
            subject: SourceSubjectMaterialV1 =
                member { project(source_subject_material) },
        );
        (
            "ConnectorSourceRecord.reacquisition",
            "SourceRecordMaterialV1.reacquisition",
            "scalar",
            "required",
            "ReacquisitionMaterialV1",
            "SourceRecordMaterialV1.reacquisition",
            reacquisition: ReacquisitionMaterialV1 =
                member { project(reacquisition_material) },
        );
        (
            "ConnectorSourceRecord.artifact_hashes",
            "SourceRecordMaterialV1.artifact_hashes",
            "artifact_id",
            "empty_array",
            "Vec<ArtifactHashMaterialV1>",
            "SourceRecordMaterialV1.artifact_hashes",
            artifact_hashes: Vec<ArtifactHashMaterialV1> = member {
                sorted_project(
                    artifact_hash_material,
                    artifact_hash_material_key
                )
            },
        );
        (
            "ConnectorSourceRecord.license",
            "SourceRecordMaterialV1.license",
            "scalar",
            "required",
            "LicenseDecisionMaterialV1",
            "SourceRecordMaterialV1.license",
            license: LicenseDecisionMaterialV1 =
                member { project(license_material) },
        );
        (
            "ConnectorSourceRecord.notice",
            "SourceRecordMaterialV1.notice",
            "scalar",
            "required",
            "NoticeMaterialV1",
            "SourceRecordMaterialV1.notice",
            notice: NoticeMaterialV1 = member { project(notice_material) },
        );
        (
            "ConnectorSourceRecord.entrypoints",
            "SourceRecordMaterialV1.entrypoints",
            "declared",
            "empty_array",
            "Vec<SourcePath>",
            "SourceRecordMaterialV1.entrypoints",
            entrypoints: Vec<String> = member { strings },
        );
        (
            "ConnectorSourceRecord.dependencies",
            "SourceRecordMaterialV1.dependencies",
            "dependency",
            "empty_array",
            "Vec<DependencyDecisionMaterialV1>",
            "SourceRecordMaterialV1.dependencies",
            dependencies: Vec<DependencyDecisionMaterialV1> = member {
                unique_sorted_project(
                    dependency_decision_material,
                    dependency_decision_material_key
                )
            },
        );
        (
            "ConnectorSourceRecord.embedded_material",
            "SourceRecordMaterialV1.embedded_material",
            "material_id",
            "empty_array",
            "Vec<EmbeddedDecisionMaterialV1>",
            "SourceRecordMaterialV1.embedded_material",
            embedded_material: Vec<EmbeddedDecisionMaterialV1> = member {
                unique_sorted_project(
                    embedded_decision_material,
                    embedded_decision_material_key
                )
            },
        );
        (
            "ConnectorSourceRecord.provider_contracts",
            "SourceRecordMaterialV1.provider_contracts",
            "contract_id",
            "empty_array",
            "Vec<ProviderContractMaterialV1>",
            "SourceRecordMaterialV1.provider_contracts",
            provider_contracts: Vec<ProviderContractMaterialV1> = member {
                unique_sorted_project(
                    provider_contract_material,
                    provider_contract_material_key
                )
            },
        );
        (
            "ConnectorSourceRecord.compatibility",
            "SourceRecordMaterialV1.compatibility",
            "scalar",
            "required",
            "CompatibilityMaterialV1",
            "SourceRecordMaterialV1.compatibility",
            compatibility: CompatibilityMaterialV1 =
                member { project(compatibility_material) },
        );
        (
            "ConnectorSourceRecord.admission",
            "SourceRecordMaterialV1.admission",
            "scalar",
            "required",
            "AdmissionMaterialV1",
            "SourceRecordMaterialV1.admission",
            admission: AdmissionMaterialV1 =
                member { project(admission_material) },
        );
        (
            "ConnectorSourceRecord.safety_findings",
            "SourceRecordMaterialV1.safety_findings",
            "scalar",
            "required",
            "SafetyFindingsMaterialV1",
            "SourceRecordMaterialV1.safety_findings",
            safety_findings: SafetyFindingsMaterialV1 =
                member { project(safety_findings_material) },
        );
        (
            "ConnectorSourceRecord.reviewer",
            "SourceRecordMaterialV1.reviewer",
            "scalar",
            "required",
            "ReviewIdentity",
            "SourceRecordMaterialV1.reviewer",
            reviewer: String = member { to_string },
        );
        (
            "ConnectorSourceRecord.approval_date",
            "SourceRecordMaterialV1.approval_date",
            "scalar",
            "required",
            "Date",
            "SourceRecordMaterialV1.approval_date",
            approval_date: String = member { to_string },
        );
        (
            "ConnectorSourceRecord.proposed_manifest",
            "SourceRecordMaterialV1.proposed_manifest",
            "scalar",
            "explicit_null",
            "Option<RepoPath>",
            "SourceRecordMaterialV1.proposed_manifest",
            proposed_manifest: Option<String> =
                member { optional_to_string },
        );
        (
            "ConnectorSourceRecord.proposed_destinations",
            "SourceRecordMaterialV1.proposed_destinations",
            "lexical",
            "nonempty_array",
            "NonEmptyVec<RepoPath>",
            "SourceRecordMaterialV1.proposed_destinations",
            proposed_destinations: Vec<String> =
                member { sorted_unique_strings },
        );
        (
            "ConnectorSourceRecord.red_tests",
            "SourceRecordMaterialV1.red_tests",
            "lexical",
            "nonempty_array",
            "NonEmptyVec<TestId>",
            "SourceRecordMaterialV1.red_tests",
            red_tests: Vec<String> = member { sorted_unique_strings },
        );
    }
    structs {
        struct ExactNpmMaterialV1 from (crate::ExactNpmPackage)
            using exact_npm_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                ];
            }
        {
            (
                "ExactNpmPackage.name",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.name",
                "scalar",
                "required",
                "string",
                "ExactNpmMaterialV1.name",
                name: String = member { clone },
            );
            (
                "ExactNpmPackage.version",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.version",
                "scalar",
                "required",
                "ExactSemver",
                "ExactNpmMaterialV1.version",
                version: String = member { as_str_owned },
            );
            (
                "ExactNpmPackage.tarball_url",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.tarball_url",
                "scalar",
                "required",
                "ExactHttpsUrl",
                "ExactNpmMaterialV1.tarball_url",
                tarball_url: String = member { to_string },
            );
            (
                "ExactNpmPackage.integrity",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.integrity",
                "scalar",
                "required",
                "NpmIntegrity",
                "ExactNpmMaterialV1.integrity",
                integrity: NpmIntegrity =
                    member { project(npm_integrity_material) },
            );
            (
                "ExactNpmPackage.repository",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.repository",
                "scalar",
                "required",
                "ImmutableRepository",
                "ExactNpmMaterialV1.repository",
                repository: ImmutableRepository =
                    member { project(immutable_repository_material) },
            );
            (
                "ExactNpmPackage.npm_git_head",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.npm_git_head",
                "scalar",
                "required",
                "GitCommit",
                "ExactNpmMaterialV1.npm_git_head",
                npm_git_head: String = member { to_string },
            );
            (
                "ExactNpmPackage.package_repository",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.package_repository",
                "scalar",
                "required",
                "RepositoryUrl",
                "ExactNpmMaterialV1.package_repository",
                package_repository: String = member { to_string },
            );
            (
                "ExactNpmPackage.signature",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.signature",
                "scalar",
                "required",
                "NpmSignatureMaterialV1",
                "ExactNpmMaterialV1.signature",
                signature: NpmSignatureMaterialV1 =
                    member { project(npm_signature_material) },
            );
            (
                "ExactNpmPackage.provenance",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.provenance",
                "scalar",
                "required",
                "NpmProvenanceMaterialV1",
                "ExactNpmMaterialV1.provenance",
                provenance: NpmProvenanceMaterialV1 =
                    member { project(npm_provenance_material) },
            );
            (
                "ExactNpmPackage.tag_commit",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.tag_commit",
                "scalar",
                "explicit_null",
                "Option<GitCommit>",
                "ExactNpmMaterialV1.tag_commit",
                tag_commit: Option<String> = member { optional_to_string },
            );
            (
                "ExactNpmPackage.provenance_commit",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.provenance_commit",
                "scalar",
                "explicit_null",
                "Option<GitCommit>",
                "ExactNpmMaterialV1.provenance_commit",
                provenance_commit: Option<String> =
                    member { optional_to_string },
            );
            (
                "ExactNpmPackage.maintainers",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.maintainers",
                "identity",
                "empty_array",
                "Vec<NpmMaintainerIdentity>",
                "ExactNpmMaterialV1.maintainers",
                maintainers: Vec<String> =
                    member { sorted_unique_strings },
            );
            (
                "ExactNpmPackage.repository_owner",
                "SourceSubjectMaterialV1{kind=exact_npm}.value.repository_owner",
                "scalar",
                "required",
                "RepositoryOwnerMaterialV1",
                "ExactNpmMaterialV1.repository_owner",
                repository_owner: RepositoryOwnerMaterialV1 =
                    member { project(repository_owner_material) },
            );
        }
        struct NpmIntegrity from (crate::NpmIntegrity)
            using npm_integrity_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("integrity"),
                ];
            }
        {
            (
                "NpmIntegrity.digest",
                "NpmIntegrity.digest",
                "scalar",
                "required",
                "bytes64",
                "NpmIntegrity.digest",
                digest: String = whole { base64url },
            );
            singleton_enum_fields {
                (
                    "NpmIntegrity.algorithm",
                    "NpmIntegrity.algorithm",
                    "scalar",
                    "required",
                    "sha512",
                    "NpmIntegrity.algorithm",
                    algorithm: NpmIntegrityAlgorithmMaterialV1
                        from (crate::source::NpmIntegrityAlgorithm)
                        using npm_integrity_algorithm_material
                        via algorithm {
                            Sha512 as "sha512";
                        },
                    Singleton,
                );
            }
        }
        struct ImmutableRepository from (crate::ImmutableRepository)
            using immutable_repository_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("repository"),
                ];
            }
        {
            (
                "ImmutableRepository.url",
                "ImmutableRepository.url",
                "scalar",
                "required",
                "RepositoryUrl",
                "ImmutableRepository.url",
                url: String = member { to_string },
            );
            (
                "ImmutableRepository.commit",
                "ImmutableRepository.commit",
                "scalar",
                "required",
                "GitCommit",
                "ImmutableRepository.commit",
                commit: String = member { to_string },
            );
            (
                "ImmutableRepository.tree",
                "ImmutableRepository.tree",
                "scalar",
                "required",
                "GitTree",
                "ImmutableRepository.tree",
                tree: String = member { to_string },
            );
        }
        struct VerifiedNpmSignatureMaterialV1
            from (crate::VerifiedNpmSignature)
            using verified_npm_signature_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("signature"),
                    source_mount_tagged!("verified"),
                    source_mount_field!("signatures"),
                    source_mount_key!([field "key_id"]),
                ];
            }
        {
            (
                "VerifiedNpmSignature.key_id",
                "VerifiedNpmSignatureMaterialV1.key_id",
                "scalar",
                "required",
                "Id",
                "VerifiedNpmSignatureMaterialV1.key_id",
                key_id: String = member { clone },
            );
            (
                "VerifiedNpmSignature.signature_sha256",
                "VerifiedNpmSignatureMaterialV1.signature_sha256",
                "scalar",
                "required",
                "Hash256",
                "VerifiedNpmSignatureMaterialV1.signature_sha256",
                signature_sha256: String = member { to_string },
            );
        }
        struct ProviderArtifactMaterialV1
            from (crate::ExactProviderArtifact)
            using provider_artifact_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                ];
            }
        {
            (
                "ExactProviderArtifact.provider",
                "SourceSubjectMaterialV1{kind=provider_artifact}.value.provider",
                "scalar",
                "required",
                "string",
                "ProviderArtifactMaterialV1.provider",
                provider: String = member { clone },
            );
            (
                "ExactProviderArtifact.evidence",
                "SourceSubjectMaterialV1{kind=provider_artifact}.value.evidence",
                "canonical_source_identity_content_sha256",
                "nonempty_array",
                "NonEmptyVec<ProviderEvidenceMaterialV1>",
                "ProviderArtifactMaterialV1.evidence",
                evidence: Vec<ProviderEvidenceMaterialV1> = member {
                    sorted_project(
                        provider_evidence_artifact_material,
                        provider_evidence_material_key
                    )
                },
            );
        }
        struct ProviderEvidenceMaterialV1
            from (crate::ProviderEvidenceArtifact)
            using provider_evidence_artifact_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                ];
            }
        {
            (
                "ProviderEvidenceArtifact.source",
                "ProviderEvidenceMaterialV1.source",
                "scalar",
                "required",
                "ImmutableProviderEvidenceSource",
                "ProviderEvidenceMaterialV1.source",
                source: ProviderEvidenceSourceMaterialV1 =
                    member { project(provider_evidence_source_material) },
            );
            (
                "ProviderEvidenceArtifact.accessed_on",
                "ProviderEvidenceMaterialV1.accessed_on",
                "scalar",
                "required",
                "Date",
                "ProviderEvidenceMaterialV1.accessed_on",
                accessed_on: String = member { to_string },
            );
            (
                "ProviderEvidenceArtifact.content_sha256",
                "ProviderEvidenceMaterialV1.content_sha256",
                "scalar",
                "required",
                "Hash256",
                "ProviderEvidenceMaterialV1.content_sha256",
                content_sha256: String = member { to_string },
            );
            (
                "ProviderEvidenceArtifact.terms",
                "ProviderEvidenceMaterialV1.terms",
                "scalar",
                "required",
                "EvidenceTermsMaterialV1",
                "ProviderEvidenceMaterialV1.terms",
                terms: EvidenceTermsMaterialV1 =
                    member { project(evidence_terms_material) },
            );
            (
                "ProviderEvidenceArtifact.facts",
                "ProviderEvidenceMaterialV1.facts",
                "fact_id",
                "nonempty_array",
                "NonEmptyVec<ProviderFactMaterialV1>",
                "ProviderEvidenceMaterialV1.facts",
                facts: Vec<ProviderFactMaterialV1> = member {
                    unique_sorted_project(
                        provider_fact_material,
                        provider_fact_material_key
                    )
                },
            );
        }
        struct ProviderFactMaterialV1 from (crate::ProviderFact)
            using provider_fact_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                    source_mount_field!("facts"),
                    source_mount_key!([field "fact_id"]),
                ];
            }
        {
            (
                "ProviderFact.fact_id",
                "ProviderFactMaterialV1.fact_id",
                "scalar",
                "required",
                "ProviderFactId",
                "ProviderFactMaterialV1.fact_id",
                fact_id: String = member { as_str_owned },
            );
            (
                "ProviderFact.location",
                "ProviderFactMaterialV1.location",
                "scalar",
                "required",
                "ExactFactLocationMaterialV1",
                "ProviderFactMaterialV1.location",
                location: ExactFactLocationMaterialV1 =
                    member { project(fact_location_material) },
            );
            (
                "ProviderFact.normalized_value",
                "ProviderFactMaterialV1.normalized_value",
                "scalar",
                "required",
                "TypedValueMaterialV1",
                "ProviderFactMaterialV1.normalized_value",
                #[serde(
                    deserialize_with =
                        "crate::source::deserialize_typed_value_material"
                )]
                normalized_value: TypedValueMaterialV1 =
                    member { clone },
            );
        }
        struct DonatOwnedMaterialV1 from (crate::DonatOwnedSource)
            using donat_owned_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("donat_owned"),
                ];
            }
        {
            (
                "DonatOwnedSource.repository_commit",
                "SourceSubjectMaterialV1{kind=donat_owned}.value.repository_commit",
                "scalar",
                "required",
                "GitCommit",
                "DonatOwnedMaterialV1.repository_commit",
                repository_commit: String = member { to_string },
            );
            (
                "DonatOwnedSource.files",
                "SourceSubjectMaterialV1{kind=donat_owned}.value.files",
                "path",
                "nonempty_array",
                "NonEmptyVec<RepoFileHash>",
                "DonatOwnedMaterialV1.files",
                files: Vec<RepoFileHashMaterialV1> = member {
                    sorted_project(
                        repo_file_hash_material,
                        repo_file_hash_material_key
                    )
                },
            );
        }
        struct RepoFileHashMaterialV1 from (crate::RepoFileHash)
            using repo_file_hash_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("donat_owned"),
                    source_mount_field!("files"),
                    source_mount_key!([field "path"]),
                ];
            }
        {
            (
                "RepoFileHash.path",
                "RepoFileHashMaterialV1.path",
                "scalar",
                "required",
                "RepoPath",
                "RepoFileHashMaterialV1.path",
                path: String = member { to_string },
            );
            (
                "RepoFileHash.sha256",
                "RepoFileHashMaterialV1.sha256",
                "scalar",
                "required",
                "Hash256",
                "RepoFileHashMaterialV1.sha256",
                sha256: String = member { to_string },
            );
        }
        struct ArtifactHashMaterialV1 from (ArtifactHash)
            using artifact_hash_material
            mounts {
                [
                    source_mount_field!("artifact_hashes"),
                    source_mount_key!([field "artifact_id"]),
                ];
            }
        {
            (
                "ArtifactHash.artifact_id",
                "ArtifactHashMaterialV1.artifact_id",
                "scalar",
                "required",
                "ArtifactId",
                "ArtifactHashMaterialV1.artifact_id",
                artifact_id: String = member { to_string },
            );
            (
                "ArtifactHash.algorithm",
                "ArtifactHashMaterialV1.algorithm",
                "scalar",
                "required",
                "HashAlgorithm",
                "ArtifactHashMaterialV1.algorithm",
                algorithm: HashAlgorithmMaterialV1 =
                    member { project(hash_algorithm_material) },
            );
            (
                "ArtifactHash.digest",
                "ArtifactHashMaterialV1.digest",
                "scalar",
                "required",
                "Hash256_or_Hash512",
                "ArtifactHashMaterialV1.digest",
                digest: String = member { clone },
            );
            (
                "ArtifactHash.path",
                "ArtifactHashMaterialV1.path",
                "scalar",
                "explicit_null",
                "Option<SourcePath>",
                "ArtifactHashMaterialV1.path",
                path: Option<String> = member { optional_to_string },
            );
        }
        struct NoticeMaterialV1 from (crate::NoticeIdentity)
            using notice_material
            mounts {
                [source_mount_field!("notice")];
            }
        {
            (
                "NoticeIdentity.id",
                "NoticeMaterialV1.id",
                "scalar",
                "required",
                "NoticeId",
                "NoticeMaterialV1.id",
                id: String = member { as_str_owned },
            );
            (
                "NoticeIdentity.license_file_path",
                "NoticeMaterialV1.license_file_path",
                "scalar",
                "required",
                "SourcePath",
                "NoticeMaterialV1.license_file_path",
                license_file_path: String = member { to_string },
            );
            (
                "NoticeIdentity.license_file_sha256",
                "NoticeMaterialV1.license_file_sha256",
                "scalar",
                "required",
                "Hash256",
                "NoticeMaterialV1.license_file_sha256",
                license_file_sha256: String = member { to_string },
            );
            (
                "NoticeIdentity.required_copyright_lines",
                "NoticeMaterialV1.required_copyright_lines",
                "declared",
                "empty_array",
                "Vec<string>",
                "NoticeMaterialV1.required_copyright_lines",
                required_copyright_lines: Vec<String> = member { clone },
            );
            (
                "NoticeIdentity.notice_bundle_destination",
                "NoticeMaterialV1.notice_bundle_destination",
                "scalar",
                "required",
                "RepoPath",
                "NoticeMaterialV1.notice_bundle_destination",
                notice_bundle_destination: String = member { to_string },
            );
        }
        struct DependencyDecisionMaterialV1
            from (crate::DependencyDecision)
            using dependency_decision_material
            mounts {
                [
                    source_mount_field!("dependencies"),
                    source_mount_key!([field "dependency"]),
                ];
            }
        {
            (
                "DependencyDecision.dependency",
                "DependencyDecisionMaterialV1.dependency",
                "scalar",
                "required",
                "Id",
                "DependencyDecisionMaterialV1.dependency",
                dependency: String = member { clone },
            );
            (
                "DependencyDecision.disposition",
                "DependencyDecisionMaterialV1.disposition",
                "scalar",
                "required",
                "DependencyDispositionMaterialV1",
                "DependencyDecisionMaterialV1.disposition",
                disposition: DependencyDispositionMaterialV1 =
                    member { project(dependency_disposition_material) },
            );
        }
        struct EmbeddedDecisionMaterialV1
            from (crate::EmbeddedMaterialDecision)
            using embedded_decision_material
            mounts {
                [
                    source_mount_field!("embedded_material"),
                    source_mount_key!([field "material_id"]),
                ];
            }
        {
            (
                "EmbeddedMaterialDecision.material_id",
                "EmbeddedDecisionMaterialV1.material_id",
                "scalar",
                "required",
                "Id",
                "EmbeddedDecisionMaterialV1.material_id",
                material_id: String = member { clone },
            );
            (
                "EmbeddedMaterialDecision.path",
                "EmbeddedDecisionMaterialV1.path",
                "scalar",
                "required",
                "SourcePath",
                "EmbeddedDecisionMaterialV1.path",
                path: String = member { to_string },
            );
            (
                "EmbeddedMaterialDecision.sha256",
                "EmbeddedDecisionMaterialV1.sha256",
                "scalar",
                "required",
                "Hash256",
                "EmbeddedDecisionMaterialV1.sha256",
                sha256: String = member { to_string },
            );
            (
                "EmbeddedMaterialDecision.disposition",
                "EmbeddedDecisionMaterialV1.disposition",
                "scalar",
                "required",
                "EmbeddedMaterialDispositionMaterialV1",
                "EmbeddedDecisionMaterialV1.disposition",
                disposition: EmbeddedMaterialDispositionMaterialV1 =
                    member { project(embedded_disposition_material) },
            );
        }
        struct ProviderContractMaterialV1
            from (ProviderContractReference)
            using provider_contract_material
            mounts {
                [
                    source_mount_field!("provider_contracts"),
                    source_mount_key!([field "contract_id"]),
                ];
            }
        {
            (
                "ProviderContractReference.contract_id",
                "ProviderContractMaterialV1.contract_id",
                "scalar",
                "required",
                "ProviderContractId",
                "ProviderContractMaterialV1.contract_id",
                contract_id: String = member { as_str_owned },
            );
            (
                "ProviderContractReference.facts",
                "ProviderContractMaterialV1.facts",
                "kind_then_fact_or_policy_id",
                "nonempty_array",
                "NonEmptyVec<ContractFactMaterialV1>",
                "ProviderContractMaterialV1.facts",
                facts: Vec<ContractFactMaterialV1> = member {
                    sorted_project(
                        contract_fact_material,
                        contract_fact_material_key
                    )
                },
            );
        }
        struct SafetyFindingsMaterialV1 from (crate::SafetyFindings)
            using safety_findings_material
            mounts {
                [source_mount_field!("safety_findings")];
            }
        {
            (
                "SafetyFindings.findings",
                "SafetyFindingsMaterialV1.findings",
                "finding_id",
                "empty_array",
                "Vec<SafetyFindingMaterialV1>",
                "SafetyFindingsMaterialV1.findings",
                findings: Vec<SafetyFindingMaterialV1> = member {
                    unique_sorted_project(
                        safety_finding_material,
                        safety_finding_material_key
                    )
                },
            );
        }
        struct SafetyFindingMaterialV1 from (crate::SafetyFinding)
            using safety_finding_material
            mounts {
                [
                    source_mount_field!("safety_findings"),
                    source_mount_field!("findings"),
                    source_mount_key!([field "finding_id"]),
                ];
            }
        {
            (
                "SafetyFinding.finding_id",
                "SafetyFindingMaterialV1.finding_id",
                "scalar",
                "required",
                "FindingId",
                "SafetyFindingMaterialV1.finding_id",
                finding_id: String = member { to_string },
            );
            (
                "SafetyFinding.kind",
                "SafetyFindingMaterialV1.kind",
                "scalar",
                "required",
                "Id",
                "SafetyFindingMaterialV1.kind",
                kind: String = member { clone },
            );
            (
                "SafetyFinding.location",
                "SafetyFindingMaterialV1.location",
                "scalar",
                "explicit_null",
                "Option<SourcePath>",
                "SafetyFindingMaterialV1.location",
                location: Option<String> = member { optional_to_string },
            );
            (
                "SafetyFinding.message",
                "SafetyFindingMaterialV1.message",
                "scalar",
                "required",
                "string",
                "SafetyFindingMaterialV1.message",
                message: String = member { clone },
            );
        }
    }
    tagged_enums {
        enum SourceSubjectMaterialV1 from (SourceSubject)
            using source_subject_material
            mounts {
                [source_mount_field!("subject")];
            }
        {
            unit_variants {}
            tuple_variants {
                (
                    "SourceSubject::ExactNpm",
                    "SourceSubjectMaterialV1{kind=exact_npm}.kind",
                    "scalar",
                    "required",
                    "exact_npm",
                    "SourceSubjectMaterialV1::ExactNpm",
                    ExactNpm as "exact_npm" (ExactNpmMaterialV1) =
                        exact_npm_material,
                );
                (
                    "SourceSubject::ProviderArtifact",
                    "SourceSubjectMaterialV1{kind=provider_artifact}.kind",
                    "scalar",
                    "required",
                    "provider_artifact",
                    "SourceSubjectMaterialV1::ProviderArtifact",
                    ProviderArtifact as "provider_artifact"
                        (ProviderArtifactMaterialV1) =
                        provider_artifact_material,
                );
                (
                    "SourceSubject::DonatOwned",
                    "SourceSubjectMaterialV1{kind=donat_owned}.kind",
                    "scalar",
                    "required",
                    "donat_owned",
                    "SourceSubjectMaterialV1::DonatOwned",
                    DonatOwned as "donat_owned" (DonatOwnedMaterialV1) =
                        donat_owned_material,
                );
            }
            struct_variants {}
        }
        enum NpmSignatureMaterialV1 from (crate::NpmSignatureDecision)
            using npm_signature_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("signature"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "NpmSignatureDecision::Verified",
                    "NpmSignatureMaterialV1{kind=verified}.kind",
                    "scalar",
                    "required",
                    "verified",
                    "NpmSignatureMaterialV1::Verified",
                    Verified as "verified" {
                        (
                            "NpmSignatureDecision::Verified.signatures",
                            "NpmSignatureMaterialV1{kind=verified}.value.signatures",
                            "key_id",
                            "nonempty_array",
                            "NonEmptyVec<VerifiedNpmSignature>",
                            "NpmSignatureMaterialV1::Verified.signatures",
                            signatures:
                                Vec<VerifiedNpmSignatureMaterialV1> = {
                                unique_sorted_project(
                                    verified_npm_signature_material,
                                    verified_npm_signature_material_key
                                )
                            },
                        );
                        (
                            "NpmSignatureDecision::Verified.registry_metadata_sha256",
                            "NpmSignatureMaterialV1{kind=verified}.value.registry_metadata_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "NpmSignatureMaterialV1::Verified.registry_metadata_sha256",
                            registry_metadata_sha256: String = { to_string },
                        );
                    },
                );
                (
                    "NpmSignatureDecision::VerifiedAbsent",
                    "NpmSignatureMaterialV1{kind=verified_absent}.kind",
                    "scalar",
                    "required",
                    "verified_absent",
                    "NpmSignatureMaterialV1::VerifiedAbsent",
                    VerifiedAbsent as "verified_absent" {
                        (
                            "NpmSignatureDecision::VerifiedAbsent.registry_metadata_sha256",
                            "NpmSignatureMaterialV1{kind=verified_absent}.value.registry_metadata_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "NpmSignatureMaterialV1::VerifiedAbsent.registry_metadata_sha256",
                            registry_metadata_sha256: String = { to_string },
                        );
                    },
                );
                (
                    "NpmSignatureDecision::Rejected",
                    "NpmSignatureMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "NpmSignatureMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "NpmSignatureDecision::Rejected.finding",
                            "NpmSignatureMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "NpmSignatureMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                        );
                    },
                );
            }
        }
        enum NpmProvenanceMaterialV1
            from (crate::NpmProvenanceDecision)
            using npm_provenance_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("provenance"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "NpmProvenanceDecision::Verified",
                    "NpmProvenanceMaterialV1{kind=verified}.kind",
                    "scalar",
                    "required",
                    "verified",
                    "NpmProvenanceMaterialV1::Verified",
                    Verified as "verified" {
                        (
                            "NpmProvenanceDecision::Verified.statement_sha256",
                            "NpmProvenanceMaterialV1{kind=verified}.value.statement_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "NpmProvenanceMaterialV1::Verified.statement_sha256",
                            statement_sha256: String = { to_string },
                        );
                        (
                            "NpmProvenanceDecision::Verified.source_commit",
                            "NpmProvenanceMaterialV1{kind=verified}.value.source_commit",
                            "scalar",
                            "required",
                            "GitCommit",
                            "NpmProvenanceMaterialV1::Verified.source_commit",
                            source_commit: String = { to_string },
                        );
                    },
                );
                (
                    "NpmProvenanceDecision::VerifiedAbsent",
                    "NpmProvenanceMaterialV1{kind=verified_absent}.kind",
                    "scalar",
                    "required",
                    "verified_absent",
                    "NpmProvenanceMaterialV1::VerifiedAbsent",
                    VerifiedAbsent as "verified_absent" {
                        (
                            "NpmProvenanceDecision::VerifiedAbsent.registry_metadata_sha256",
                            "NpmProvenanceMaterialV1{kind=verified_absent}.value.registry_metadata_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "NpmProvenanceMaterialV1::VerifiedAbsent.registry_metadata_sha256",
                            registry_metadata_sha256: String = { to_string },
                        );
                    },
                );
                (
                    "NpmProvenanceDecision::Rejected",
                    "NpmProvenanceMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "NpmProvenanceMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "NpmProvenanceDecision::Rejected.finding",
                            "NpmProvenanceMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "NpmProvenanceMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                        );
                    },
                );
            }
        }
        enum RepositoryOwnerMaterialV1
            from (crate::RepositoryOwnerDecision)
            using repository_owner_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("exact_npm"),
                    source_mount_field!("repository_owner"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "RepositoryOwnerDecision::Consistent",
                    "RepositoryOwnerMaterialV1{kind=consistent}.kind",
                    "scalar",
                    "required",
                    "consistent",
                    "RepositoryOwnerMaterialV1::Consistent",
                    Consistent as "consistent" {
                        (
                            "RepositoryOwnerDecision::Consistent.package_owner",
                            "RepositoryOwnerMaterialV1{kind=consistent}.value.package_owner",
                            "scalar",
                            "required",
                            "NpmOwnerIdentity",
                            "RepositoryOwnerMaterialV1::Consistent.package_owner",
                            package_owner: String = { to_string },
                        );
                        (
                            "RepositoryOwnerDecision::Consistent.repository_owner",
                            "RepositoryOwnerMaterialV1{kind=consistent}.value.repository_owner",
                            "scalar",
                            "required",
                            "RepositoryOwnerIdentity",
                            "RepositoryOwnerMaterialV1::Consistent.repository_owner",
                            repository_owner: String = { to_string },
                        );
                    },
                );
                (
                    "RepositoryOwnerDecision::ReviewedMismatch",
                    "RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.kind",
                    "scalar",
                    "required",
                    "reviewed_mismatch",
                    "RepositoryOwnerMaterialV1::ReviewedMismatch",
                    ReviewedMismatch as "reviewed_mismatch" {
                        (
                            "RepositoryOwnerDecision::ReviewedMismatch.decision_id",
                            "RepositoryOwnerMaterialV1{kind=reviewed_mismatch}.value.decision_id",
                            "scalar",
                            "required",
                            "ReviewDecisionId",
                            "RepositoryOwnerMaterialV1::ReviewedMismatch.decision_id",
                            decision_id: String = { to_string },
                        );
                    },
                );
                (
                    "RepositoryOwnerDecision::Rejected",
                    "RepositoryOwnerMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "RepositoryOwnerMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "RepositoryOwnerDecision::Rejected.finding",
                            "RepositoryOwnerMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "RepositoryOwnerMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                        );
                    },
                );
            }
        }
        enum ProviderEvidenceSourceMaterialV1
            from (crate::ImmutableProviderEvidenceSource)
            using provider_evidence_source_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                    source_mount_field!("source"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "ImmutableProviderEvidenceSource::RepositoryFile",
                    "ProviderEvidenceSourceMaterialV1{kind=repository_file}.kind",
                    "scalar",
                    "required",
                    "repository_file",
                    "ProviderEvidenceSourceMaterialV1::RepositoryFile",
                    RepositoryFile as "repository_file" {
                        (
                            "ImmutableProviderEvidenceSource::RepositoryFile.repository",
                            "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.repository",
                            "scalar",
                            "required",
                            "RepositoryUrl",
                            "ProviderEvidenceSourceMaterialV1::RepositoryFile.repository",
                            repository: String = { to_string },
                        );
                        (
                            "ImmutableProviderEvidenceSource::RepositoryFile.commit",
                            "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.commit",
                            "scalar",
                            "required",
                            "GitCommit",
                            "ProviderEvidenceSourceMaterialV1::RepositoryFile.commit",
                            commit: String = { to_string },
                        );
                        (
                            "ImmutableProviderEvidenceSource::RepositoryFile.path",
                            "ProviderEvidenceSourceMaterialV1{kind=repository_file}.value.path",
                            "scalar",
                            "required",
                            "SourcePath",
                            "ProviderEvidenceSourceMaterialV1::RepositoryFile.path",
                            path: String = { to_string },
                        );
                    },
                );
                (
                    "ImmutableProviderEvidenceSource::VersionedArtifact",
                    "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.kind",
                    "scalar",
                    "required",
                    "versioned_artifact",
                    "ProviderEvidenceSourceMaterialV1::VersionedArtifact",
                    VersionedArtifact as "versioned_artifact" {
                        (
                            "ImmutableProviderEvidenceSource::VersionedArtifact.url",
                            "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.url",
                            "scalar",
                            "required",
                            "ExactHttpsUrl",
                            "ProviderEvidenceSourceMaterialV1::VersionedArtifact.url",
                            url: String = { to_string },
                        );
                        (
                            "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
                            "ProviderEvidenceSourceMaterialV1{kind=versioned_artifact}.value.provider_revision",
                            "scalar",
                            "required",
                            "NonEmptyString",
                            "ProviderEvidenceSourceMaterialV1::VersionedArtifact.provider_revision",
                            provider_revision: String = { to_string },
                        );
                    },
                );
            }
        }
        enum EvidenceTermsMaterialV1
            from (crate::EvidenceTermsDisposition)
            using evidence_terms_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                    source_mount_field!("terms"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "EvidenceTermsDisposition::Permissive",
                    "EvidenceTermsMaterialV1{kind=permissive}.kind",
                    "scalar",
                    "required",
                    "permissive",
                    "EvidenceTermsMaterialV1::Permissive",
                    Permissive as "permissive" {
                        (
                            "EvidenceTermsDisposition::Permissive.license",
                            "EvidenceTermsMaterialV1{kind=permissive}.value.license",
                            "scalar",
                            "required",
                            "LicenseDecisionMaterialV1",
                            "EvidenceTermsMaterialV1::Permissive.license",
                            license: LicenseDecisionMaterialV1 = {
                                project(license_material)
                            },
                        );
                        (
                            "EvidenceTermsDisposition::Permissive.evidence_url",
                            "EvidenceTermsMaterialV1{kind=permissive}.value.evidence_url",
                            "scalar",
                            "required",
                            "ExactHttpsUrl",
                            "EvidenceTermsMaterialV1::Permissive.evidence_url",
                            evidence_url: String = { to_string },
                        );
                    },
                );
                (
                    "EvidenceTermsDisposition::ReviewedUse",
                    "EvidenceTermsMaterialV1{kind=reviewed_use}.kind",
                    "scalar",
                    "required",
                    "reviewed_use",
                    "EvidenceTermsMaterialV1::ReviewedUse",
                    ReviewedUse as "reviewed_use" {
                        (
                            "EvidenceTermsDisposition::ReviewedUse.decision_id",
                            "EvidenceTermsMaterialV1{kind=reviewed_use}.value.decision_id",
                            "scalar",
                            "required",
                            "ReviewDecisionId",
                            "EvidenceTermsMaterialV1::ReviewedUse.decision_id",
                            decision_id: String = { to_string },
                        );
                        (
                            "EvidenceTermsDisposition::ReviewedUse.evidence_url",
                            "EvidenceTermsMaterialV1{kind=reviewed_use}.value.evidence_url",
                            "scalar",
                            "required",
                            "ExactHttpsUrl",
                            "EvidenceTermsMaterialV1::ReviewedUse.evidence_url",
                            evidence_url: String = { to_string },
                        );
                    },
                );
                (
                    "EvidenceTermsDisposition::Rejected",
                    "EvidenceTermsMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "EvidenceTermsMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "EvidenceTermsDisposition::Rejected.finding",
                            "EvidenceTermsMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "EvidenceTermsMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                            LoaderRejected,
                        );
                    },
                    LoaderRejected,
                );
            }
        }
        enum ExactFactLocationMaterialV1 from (ExactFactLocation)
            using fact_location_material
            mounts {
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                    source_mount_field!("facts"),
                    source_mount_key!([field "fact_id"]),
                    source_mount_field!("location"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "ExactFactLocation::JsonPointer",
                    "ExactFactLocationMaterialV1{kind=json_pointer}.kind",
                    "scalar",
                    "required",
                    "json_pointer",
                    "ExactFactLocationMaterialV1::JsonPointer",
                    JsonPointer as "json_pointer" {
                        (
                            "ExactFactLocation::JsonPointer.path",
                            "ExactFactLocationMaterialV1{kind=json_pointer}.value.path",
                            "scalar",
                            "required",
                            "SourcePath",
                            "ExactFactLocationMaterialV1::JsonPointer.path",
                            path: String = { to_string },
                        );
                        (
                            "ExactFactLocation::JsonPointer.pointer",
                            "ExactFactLocationMaterialV1{kind=json_pointer}.value.pointer",
                            "scalar",
                            "required",
                            "StaticJsonPointer",
                            "ExactFactLocationMaterialV1::JsonPointer.pointer",
                            pointer: String = { clone },
                        );
                    },
                );
                (
                    "ExactFactLocation::DocumentSection",
                    "ExactFactLocationMaterialV1{kind=document_section}.kind",
                    "scalar",
                    "required",
                    "document_section",
                    "ExactFactLocationMaterialV1::DocumentSection",
                    DocumentSection as "document_section" {
                        (
                            "ExactFactLocation::DocumentSection.path",
                            "ExactFactLocationMaterialV1{kind=document_section}.value.path",
                            "scalar",
                            "required",
                            "SourcePath",
                            "ExactFactLocationMaterialV1::DocumentSection.path",
                            path: String = { to_string },
                        );
                        (
                            "ExactFactLocation::DocumentSection.section",
                            "ExactFactLocationMaterialV1{kind=document_section}.value.section",
                            "scalar",
                            "required",
                            "string",
                            "ExactFactLocationMaterialV1::DocumentSection.section",
                            section: String = { clone },
                        );
                    },
                );
            }
        }
        enum ReacquisitionMaterialV1 from (crate::ReacquisitionPlan)
            using reacquisition_material
            mounts {
                [source_mount_field!("reacquisition")];
            }
        {
            unit_variants {
                (
                    "ReacquisitionPlan::ExactNpmReview",
                    "ReacquisitionMaterialV1{kind=exact_npm_review}.kind",
                    "scalar",
                    "required",
                    "exact_npm_review",
                    "ReacquisitionMaterialV1::ExactNpmReview",
                    ExactNpmReview as "exact_npm_review",
                );
                (
                    "ReacquisitionPlan::ProviderRepositoryReview",
                    "ReacquisitionMaterialV1{kind=provider_repository_review}.kind",
                    "scalar",
                    "required",
                    "provider_repository_review",
                    "ReacquisitionMaterialV1::ProviderRepositoryReview",
                    ProviderRepositoryReview
                        as "provider_repository_review",
                );
                (
                    "ReacquisitionPlan::ProviderVersionedArtifactReview",
                    "ReacquisitionMaterialV1{kind=provider_versioned_artifact_review}.kind",
                    "scalar",
                    "required",
                    "provider_versioned_artifact_review",
                    "ReacquisitionMaterialV1::ProviderVersionedArtifactReview",
                    ProviderVersionedArtifactReview
                        as "provider_versioned_artifact_review",
                );
                (
                    "ReacquisitionPlan::DonatOwnedNoNetwork",
                    "ReacquisitionMaterialV1{kind=donat_owned_no_network}.kind",
                    "scalar",
                    "required",
                    "donat_owned_no_network",
                    "ReacquisitionMaterialV1::DonatOwnedNoNetwork",
                    DonatOwnedNoNetwork as "donat_owned_no_network",
                );
            }
            tuple_variants {}
            struct_variants {}
        }
        enum HashAlgorithmMaterialV1 from (crate::HashAlgorithm)
            using hash_algorithm_material
            mounts {
                [
                    source_mount_field!("artifact_hashes"),
                    source_mount_key!([field "artifact_id"]),
                    source_mount_field!("algorithm"),
                ];
            }
        {
            unit_variants {
                (
                    "HashAlgorithm::Sha256",
                    "HashAlgorithmMaterialV1{kind=sha256}.kind",
                    "scalar",
                    "required",
                    "sha256",
                    "HashAlgorithmMaterialV1::Sha256",
                    Sha256 as "sha256",
                );
                (
                    "HashAlgorithm::Sha512",
                    "HashAlgorithmMaterialV1{kind=sha512}.kind",
                    "scalar",
                    "required",
                    "sha512",
                    "HashAlgorithmMaterialV1::Sha512",
                    Sha512 as "sha512",
                );
            }
            tuple_variants {}
            struct_variants {}
        }
        enum LicenseDecisionMaterialV1 from (crate::LicenseDecision)
            using license_material
            mounts {
                [source_mount_field!("license")];
                [
                    source_mount_field!("dependencies"),
                    source_mount_key!([field "dependency"]),
                    source_mount_field!("disposition"),
                    source_mount_tagged!("shipped"),
                    source_mount_field!("license"),
                ];
                [
                    source_mount_field!("dependencies"),
                    source_mount_key!([field "dependency"]),
                    source_mount_field!("disposition"),
                    source_mount_tagged!("build_only"),
                    source_mount_field!("license"),
                ];
                [
                    source_mount_field!("embedded_material"),
                    source_mount_key!([field "material_id"]),
                    source_mount_field!("disposition"),
                    source_mount_tagged!("shipped"),
                    source_mount_field!("license"),
                ];
                [
                    source_mount_field!("subject"),
                    source_mount_tagged!("provider_artifact"),
                    source_mount_field!("evidence"),
                    source_mount_key!(
                        [field "source"],
                        [field "content_sha256"],
                    ),
                    source_mount_field!("terms"),
                    source_mount_tagged!("permissive"),
                    source_mount_field!("license"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "LicenseDecision::Permissive",
                    "LicenseDecisionMaterialV1{kind=permissive}.kind",
                    "scalar",
                    "required",
                    "permissive",
                    "LicenseDecisionMaterialV1::Permissive",
                    Permissive as "permissive" {
                        (
                            "LicenseDecision::Permissive.spdx_id",
                            "LicenseDecisionMaterialV1{kind=permissive}.value.spdx_id",
                            "scalar",
                            "required",
                            "string",
                            "LicenseDecisionMaterialV1::Permissive.spdx_id",
                            spdx_id: String = { clone },
                        );
                        (
                            "LicenseDecision::Permissive.selected_dual_license_branch",
                            "LicenseDecisionMaterialV1{kind=permissive}.value.selected_dual_license_branch",
                            "scalar",
                            "explicit_null",
                            "Option<string>",
                            "LicenseDecisionMaterialV1::Permissive.selected_dual_license_branch",
                            selected_dual_license_branch:
                                Option<String> = { clone },
                        );
                        (
                            "LicenseDecision::Permissive.license_file_path",
                            "LicenseDecisionMaterialV1{kind=permissive}.value.license_file_path",
                            "scalar",
                            "required",
                            "SourcePath",
                            "LicenseDecisionMaterialV1::Permissive.license_file_path",
                            license_file_path: String = { to_string },
                        );
                        (
                            "LicenseDecision::Permissive.license_file_sha256",
                            "LicenseDecisionMaterialV1{kind=permissive}.value.license_file_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "LicenseDecisionMaterialV1::Permissive.license_file_sha256",
                            license_file_sha256: String = { to_string },
                        );
                    },
                );
                (
                    "LicenseDecision::WrittenGrant",
                    "LicenseDecisionMaterialV1{kind=written_grant}.kind",
                    "scalar",
                    "required",
                    "written_grant",
                    "LicenseDecisionMaterialV1::WrittenGrant",
                    WrittenGrant as "written_grant" {
                        (
                            "LicenseDecision::WrittenGrant.decision_id",
                            "LicenseDecisionMaterialV1{kind=written_grant}.value.decision_id",
                            "scalar",
                            "required",
                            "ReviewDecisionId",
                            "LicenseDecisionMaterialV1::WrittenGrant.decision_id",
                            decision_id: String = { to_string },
                        );
                        (
                            "LicenseDecision::WrittenGrant.grant_sha256",
                            "LicenseDecisionMaterialV1{kind=written_grant}.value.grant_sha256",
                            "scalar",
                            "required",
                            "Hash256",
                            "LicenseDecisionMaterialV1::WrittenGrant.grant_sha256",
                            grant_sha256: String = { to_string },
                        );
                    },
                );
                (
                    "LicenseDecision::Rejected",
                    "LicenseDecisionMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "LicenseDecisionMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "LicenseDecision::Rejected.finding",
                            "LicenseDecisionMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "LicenseDecisionMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                            LoaderRejected,
                        );
                    },
                    LoaderRejected,
                );
            }
        }
        enum DependencyDispositionMaterialV1
            from (crate::DependencyDisposition)
            using dependency_disposition_material
            mounts {
                [
                    source_mount_field!("dependencies"),
                    source_mount_key!([field "dependency"]),
                    source_mount_field!("disposition"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "DependencyDisposition::Shipped",
                    "DependencyDispositionMaterialV1{kind=shipped}.kind",
                    "scalar",
                    "required",
                    "shipped",
                    "DependencyDispositionMaterialV1::Shipped",
                    Shipped as "shipped" {
                        (
                            "DependencyDisposition::Shipped.license",
                            "DependencyDispositionMaterialV1{kind=shipped}.value.license",
                            "scalar",
                            "required",
                            "LicenseDecisionMaterialV1",
                            "DependencyDispositionMaterialV1::Shipped.license",
                            license: LicenseDecisionMaterialV1 = {
                                project(license_material)
                            },
                        );
                    },
                );
                (
                    "DependencyDisposition::BuildOnly",
                    "DependencyDispositionMaterialV1{kind=build_only}.kind",
                    "scalar",
                    "required",
                    "build_only",
                    "DependencyDispositionMaterialV1::BuildOnly",
                    BuildOnly as "build_only" {
                        (
                            "DependencyDisposition::BuildOnly.license",
                            "DependencyDispositionMaterialV1{kind=build_only}.value.license",
                            "scalar",
                            "required",
                            "LicenseDecisionMaterialV1",
                            "DependencyDispositionMaterialV1::BuildOnly.license",
                            license: LicenseDecisionMaterialV1 = {
                                project(license_material)
                            },
                        );
                    },
                );
                (
                    "DependencyDisposition::TypeOnlyReplaced",
                    "DependencyDispositionMaterialV1{kind=type_only_replaced}.kind",
                    "scalar",
                    "required",
                    "type_only_replaced",
                    "DependencyDispositionMaterialV1::TypeOnlyReplaced",
                    TypeOnlyReplaced as "type_only_replaced" {
                        (
                            "DependencyDisposition::TypeOnlyReplaced.replacement",
                            "DependencyDispositionMaterialV1{kind=type_only_replaced}.value.replacement",
                            "scalar",
                            "required",
                            "Id",
                            "DependencyDispositionMaterialV1::TypeOnlyReplaced.replacement",
                            replacement: String = { clone },
                        );
                    },
                );
                (
                    "DependencyDisposition::BehaviorOnly",
                    "DependencyDispositionMaterialV1{kind=behavior_only}.kind",
                    "scalar",
                    "required",
                    "behavior_only",
                    "DependencyDispositionMaterialV1::BehaviorOnly",
                    BehaviorOnly as "behavior_only" {
                        (
                            "DependencyDisposition::BehaviorOnly.reason",
                            "DependencyDispositionMaterialV1{kind=behavior_only}.value.reason",
                            "scalar",
                            "required",
                            "FindingId",
                            "DependencyDispositionMaterialV1::BehaviorOnly.reason",
                            reason: String = { to_string },
                        );
                    },
                );
                (
                    "DependencyDisposition::Rejected",
                    "DependencyDispositionMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "DependencyDispositionMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "DependencyDisposition::Rejected.finding",
                            "DependencyDispositionMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "DependencyDispositionMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                            ExecutableRejected,
                        );
                    },
                    ExecutableRejected,
                );
            }
        }
        enum EmbeddedMaterialDispositionMaterialV1
            from (crate::EmbeddedMaterialDisposition)
            using embedded_disposition_material
            mounts {
                [
                    source_mount_field!("embedded_material"),
                    source_mount_key!([field "material_id"]),
                    source_mount_field!("disposition"),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "EmbeddedMaterialDisposition::Shipped",
                    "EmbeddedMaterialDispositionMaterialV1{kind=shipped}.kind",
                    "scalar",
                    "required",
                    "shipped",
                    "EmbeddedMaterialDispositionMaterialV1::Shipped",
                    Shipped as "shipped" {
                        (
                            "EmbeddedMaterialDisposition::Shipped.license",
                            "EmbeddedMaterialDispositionMaterialV1{kind=shipped}.value.license",
                            "scalar",
                            "required",
                            "LicenseDecisionMaterialV1",
                            "EmbeddedMaterialDispositionMaterialV1::Shipped.license",
                            license: LicenseDecisionMaterialV1 = {
                                project(license_material)
                            },
                        );
                    },
                );
                (
                    "EmbeddedMaterialDisposition::BehaviorOnly",
                    "EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.kind",
                    "scalar",
                    "required",
                    "behavior_only",
                    "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly",
                    BehaviorOnly as "behavior_only" {
                        (
                            "EmbeddedMaterialDisposition::BehaviorOnly.reason",
                            "EmbeddedMaterialDispositionMaterialV1{kind=behavior_only}.value.reason",
                            "scalar",
                            "required",
                            "FindingId",
                            "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly.reason",
                            reason: String = { to_string },
                        );
                    },
                );
                (
                    "EmbeddedMaterialDisposition::Rejected",
                    "EmbeddedMaterialDispositionMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "EmbeddedMaterialDispositionMaterialV1::Rejected",
                    Rejected as "rejected" {
                        (
                            "EmbeddedMaterialDisposition::Rejected.finding",
                            "EmbeddedMaterialDispositionMaterialV1{kind=rejected}.value.finding",
                            "scalar",
                            "required",
                            "FindingId",
                            "EmbeddedMaterialDispositionMaterialV1::Rejected.finding",
                            finding: String = { to_string },
                            ExecutableRejected,
                        );
                    },
                    ExecutableRejected,
                );
            }
        }
        enum ContractFactMaterialV1 from (ContractFact)
            using contract_fact_material
            mounts {
                [
                    source_mount_field!("provider_contracts"),
                    source_mount_key!([field "contract_id"]),
                    source_mount_field!("facts"),
                    source_mount_key!(
                        [field "kind"],
                        [
                            tagged "provider_evidence",
                            field "fact_id",
                        ],
                        [
                            tagged "donat_policy",
                            field "policy_id",
                        ],
                    ),
                ];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "ContractFact::ProviderEvidence",
                    "ContractFactMaterialV1{kind=provider_evidence}.kind",
                    "scalar",
                    "required",
                    "provider_evidence",
                    "ContractFactMaterialV1::ProviderEvidence",
                    ProviderEvidence as "provider_evidence" {
                        (
                            "ContractFact::ProviderEvidence.source_record_id",
                            "ContractFactMaterialV1{kind=provider_evidence}.value.source_record_id",
                            "scalar",
                            "required",
                            "SourceRecordId",
                            "ContractFactMaterialV1::ProviderEvidence.source_record_id",
                            source_record_id: String = { as_str_owned },
                        );
                        (
                            "ContractFact::ProviderEvidence.fact_id",
                            "ContractFactMaterialV1{kind=provider_evidence}.value.fact_id",
                            "scalar",
                            "required",
                            "ProviderFactId",
                            "ContractFactMaterialV1::ProviderEvidence.fact_id",
                            fact_id: String = { as_str_owned },
                        );
                    },
                );
                (
                    "ContractFact::DonatPolicy",
                    "ContractFactMaterialV1{kind=donat_policy}.kind",
                    "scalar",
                    "required",
                    "donat_policy",
                    "ContractFactMaterialV1::DonatPolicy",
                    DonatPolicy as "donat_policy" {
                        (
                            "ContractFact::DonatPolicy.policy_id",
                            "ContractFactMaterialV1{kind=donat_policy}.value.policy_id",
                            "scalar",
                            "required",
                            "DonatPolicyId",
                            "ContractFactMaterialV1::DonatPolicy.policy_id",
                            policy_id: String = { as_str_owned },
                        );
                        (
                            "ContractFact::DonatPolicy.value",
                            "ContractFactMaterialV1{kind=donat_policy}.value.value",
                            "scalar",
                            "required",
                            "TypedValueMaterialV1",
                            "ContractFactMaterialV1::DonatPolicy.value",
                            #[serde(
                                deserialize_with =
                                    "crate::source::deserialize_typed_value_material"
                            )]
                            value: TypedValueMaterialV1 = { clone },
                        );
                    },
                );
            }
        }
        enum CompatibilityMaterialV1
            from (crate::CompatibilityDecision)
            using compatibility_material
            mounts {
                [source_mount_field!("compatibility")];
            }
        {
            unit_variants {
                (
                    "CompatibilityDecision::TierA",
                    "CompatibilityMaterialV1{kind=tier_a}.kind",
                    "scalar",
                    "required",
                    "tier_a",
                    "CompatibilityMaterialV1::TierA",
                    TierA as "tier_a",
                );
                (
                    "CompatibilityDecision::TierB",
                    "CompatibilityMaterialV1{kind=tier_b}.kind",
                    "scalar",
                    "required",
                    "tier_b",
                    "CompatibilityMaterialV1::TierB",
                    TierB as "tier_b",
                );
                (
                    "CompatibilityDecision::TierC",
                    "CompatibilityMaterialV1{kind=tier_c}.kind",
                    "scalar",
                    "required",
                    "tier_c",
                    "CompatibilityMaterialV1::TierC",
                    TierC as "tier_c",
                );
                (
                    "CompatibilityDecision::Rejected",
                    "CompatibilityMaterialV1{kind=rejected}.kind",
                    "scalar",
                    "required",
                    "rejected",
                    "CompatibilityMaterialV1::Rejected",
                    Rejected as "rejected",
                );
            }
            tuple_variants {}
            struct_variants {}
        }
        enum AdmissionMaterialV1 from (crate::AdmissionState)
            using admission_material
            mounts {
                [source_mount_field!("admission")];
            }
        {
            unit_variants {}
            tuple_variants {}
            struct_variants {
                (
                    "AdmissionState::InventoryOnly",
                    "AdmissionMaterialV1{kind=inventory_only}.kind",
                    "scalar",
                    "required",
                    "inventory_only",
                    "AdmissionMaterialV1::InventoryOnly",
                    InventoryOnly as "inventory_only" {
                        (
                            "AdmissionState::InventoryOnly.findings",
                            "AdmissionMaterialV1{kind=inventory_only}.value.findings",
                            "lexical",
                            "nonempty_array",
                            "NonEmptyVec<FindingId>",
                            "AdmissionMaterialV1::InventoryOnly.findings",
                            findings: Vec<String> = {
                                sorted_unique_strings
                            },
                        );
                    },
                );
                (
                    "AdmissionState::ApprovedForPort",
                    "AdmissionMaterialV1{kind=approved_for_port}.kind",
                    "scalar",
                    "required",
                    "approved_for_port",
                    "AdmissionMaterialV1::ApprovedForPort",
                    ApprovedForPort as "approved_for_port" {
                        (
                            "AdmissionState::ApprovedForPort.operations",
                            "AdmissionMaterialV1{kind=approved_for_port}.value.operations",
                            "lexical",
                            "nonempty_array",
                            "NonEmptyVec<OperationId>",
                            "AdmissionMaterialV1::ApprovedForPort.operations",
                            operations: Vec<String> = {
                                sorted_unique_ids
                            },
                        );
                    },
                );
                (
                    "AdmissionState::EvidenceAccepted",
                    "AdmissionMaterialV1{kind=evidence_accepted}.kind",
                    "scalar",
                    "required",
                    "evidence_accepted",
                    "AdmissionMaterialV1::EvidenceAccepted",
                    EvidenceAccepted as "evidence_accepted" {
                        (
                            "AdmissionState::EvidenceAccepted.contracts",
                            "AdmissionMaterialV1{kind=evidence_accepted}.value.contracts",
                            "lexical",
                            "nonempty_array",
                            "NonEmptyVec<ProviderContractId>",
                            "AdmissionMaterialV1::EvidenceAccepted.contracts",
                            contracts: Vec<String> = {
                                sorted_unique_ids
                            },
                        );
                    },
                );
            }
        }
    }
}
semantic_projection {
    root pub struct SemanticMaterialV1
        using semantic_material(
            checked,
            manifest,
            canonical_schema_epoch
        ) {
        derived {
            (
                "derived::canonical_schema_epoch",
                "derived::canonical_schema_epoch",
                Constant,
                "SemanticMaterialV1.canonical_schema_epoch",
                "constant",
                "scalar",
                "required",
                "CANONICAL_SCHEMA_EPOCH",
                "SemanticMaterialV1.canonical_schema_epoch",
                canonical_schema_epoch: u32 = {
                    validate_semantic_canonical_schema_epoch(
                        canonical_schema_epoch
                    )?
                },
            );
        }
        fields {
            (
                "ConnectorManifest.credentials",
                "ConnectorManifest.credentials",
                Model,
                "SemanticMaterialV1.credentials",
                "normalized",
                "credential",
                "empty_array",
                "Vec<SemanticCredentialMaterialV1>",
                "SemanticMaterialV1.credentials",
                credentials: Vec<SemanticCredentialMaterialV1> = {
                    let mut values = manifest
                        .credentials
                        .iter()
                        .map(semantic_credential_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        (&left.credential, left.version)
                            .cmp(&(&right.credential, right.version))
                    });
                    values
                },
            );
            (
                "ConnectorManifest.operations",
                "ConnectorManifest.operations",
                Model,
                "SemanticMaterialV1.operations",
                "normalized",
                "operation",
                "nonempty_array",
                "NonEmptyVec<SemanticOperationMaterialV1>",
                "SemanticMaterialV1.operations",
                operations: Vec<SemanticOperationMaterialV1> = {
                    let value_language_epoch =
                        validate_semantic_value_language_epoch(
                            manifest.value_language_epoch,
                        )?;
                    let mut values = manifest
                        .operations
                        .iter()
                        .map(|operation| {
                            semantic_operation_material(
                                operation,
                                value_language_epoch,
                            )
                        })
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        (&left.operation, left.operation_version)
                            .cmp(&(
                                &right.operation,
                                right.operation_version,
                            ))
                    });
                    values
                },
            );
            (
                "ConnectorManifest.origins",
                "ConnectorManifest.origins",
                Model,
                "SemanticMaterialV1.origins",
                "normalized",
                "origin",
                "nonempty_array",
                "NonEmptyVec<SemanticOriginMaterialV1>",
                "SemanticMaterialV1.origins",
                origins: Vec<SemanticOriginMaterialV1> = {
                    let mut values = manifest
                        .origins
                        .iter()
                        .map(semantic_origin_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        left.origin.cmp(&right.origin)
                    });
                    values
                },
            );
            (
                "ConnectorManifest.triggers",
                "ConnectorManifest.triggers",
                Model,
                "SemanticMaterialV1.triggers",
                "normalized",
                "kind_then_trigger",
                "empty_array",
                "Vec<SemanticTriggerMaterialV1>",
                "SemanticMaterialV1.triggers",
                triggers: Vec<SemanticTriggerMaterialV1> = {
                    let value_language_epoch =
                        validate_semantic_value_language_epoch(
                            manifest.value_language_epoch
                        )?;
                    let mut values = manifest
                        .triggers
                        .iter()
                        .map(|trigger| {
                            semantic_trigger_material(
                                trigger,
                                value_language_epoch,
                            )
                        })
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        semantic_trigger_key(left)
                            .cmp(&semantic_trigger_key(right))
                    });
                    values
                },
            );
            (
                "ConnectorManifest.value_language_epoch",
                "ConnectorManifest.value_language_epoch",
                Model,
                "SemanticMaterialV1.value_language_epoch",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "SemanticMaterialV1.value_language_epoch",
                value_language_epoch: u32 = {
                    validate_semantic_value_language_epoch(
                        manifest.value_language_epoch
                    )?
                },
            );
        }
        composite_fields {
            (
                connector: SemanticConnectorMaterialV1 = {
                    semantic_connector_material(manifest)?
                },
            );
        }
    }
    structs {
        pub struct SemanticConnectorMaterialV1
            from (crate::ConnectorManifest)
            using semantic_connector_material(manifest) {
            (
                "ConnectorManifest.api_identity",
                "ConnectorManifest.api_identity",
                Model,
                "SemanticMaterialV1.connector.api_identity",
                "normalized",
                "scalar",
                "required",
                "ApiIdentity",
                "SemanticConnectorMaterialV1.api_identity",
                api_identity: String = {
                    manifest.api_identity.clone()
                },
            );
            (
                "ConnectorManifest.connector",
                "ConnectorManifest.connector",
                Model,
                "SemanticMaterialV1.connector.id",
                "normalized",
                "scalar",
                "required",
                "ConnectorId",
                "SemanticConnectorMaterialV1.id",
                id: String = {
                    manifest.connector.as_str().to_owned()
                },
            );
            (
                "ConnectorManifest.manifest_version",
                "ConnectorManifest.manifest_version",
                Model,
                "SemanticMaterialV1.connector.manifest_version",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "SemanticConnectorMaterialV1.manifest_version",
                manifest_version: u32 = {
                    manifest.manifest_version
                },
            );
            (
                "ConnectorManifest.provider",
                "ConnectorManifest.provider",
                Model,
                "SemanticMaterialV1.connector.provider",
                "normalized",
                "scalar",
                "required",
                "ProviderId",
                "SemanticConnectorMaterialV1.provider",
                provider: String = {
                    manifest.provider.clone()
                },
            );
            (
                "ConnectorManifest.runtime_abi_epoch",
                "ConnectorManifest.runtime_abi_epoch",
                Model,
                "SemanticMaterialV1.connector.runtime_abi_epoch",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "SemanticConnectorMaterialV1.runtime_abi_epoch",
                runtime_abi_epoch: u32 = {
                    manifest.runtime_abi_epoch
                },
            );
            (
                "ConnectorManifest.connector_version",
                "ConnectorManifest.connector_version",
                Model,
                "SemanticMaterialV1.connector.version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "SemanticConnectorMaterialV1.version",
                version: StableSemver = {
                    manifest.connector_version
                },
            );
        }
        struct CredentialBoundsMaterialV1
            from (crate::CredentialBounds)
            using project_credential_bounds_material(value) {
            (
                "CredentialBounds.maximum_field_bytes",
                "CredentialBounds.maximum_field_bytes",
                Model,
                "CredentialBoundsMaterialV1.maximum_field_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "CredentialBoundsMaterialV1.maximum_field_bytes",
                maximum_field_bytes: u32 = {
                    value.maximum_field_bytes.get()
                },
            );
            (
                "CredentialBounds.maximum_aggregate_bytes",
                "CredentialBounds.maximum_aggregate_bytes",
                Model,
                "CredentialBoundsMaterialV1.maximum_aggregate_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "CredentialBoundsMaterialV1.maximum_aggregate_bytes",
                maximum_aggregate_bytes: u32 = {
                    value.maximum_aggregate_bytes.get()
                },
            );
            (
                "CredentialBounds.maximum_token_bytes",
                "CredentialBounds.maximum_token_bytes",
                Model,
                "CredentialBoundsMaterialV1.maximum_token_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "CredentialBoundsMaterialV1.maximum_token_bytes",
                maximum_token_bytes: u32 = {
                    value.maximum_token_bytes.get()
                },
            );
        }
        struct VersionedOperationReferenceMaterialV1
            from (crate::VersionedOperationReference)
            using project_versioned_operation_reference_material(value) {
            (
                "VersionedOperationReference.operation",
                "VersionedOperationReference.operation",
                Model,
                "VersionedOperationReferenceMaterialV1.operation",
                "normalized",
                "scalar",
                "required",
                "OperationId",
                "VersionedOperationReferenceMaterialV1.operation",
                operation: String = {
                    value.operation.as_str().to_owned()
                },
            );
            (
                "VersionedOperationReference.version",
                "VersionedOperationReference.version",
                Model,
                "VersionedOperationReferenceMaterialV1.version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "VersionedOperationReferenceMaterialV1.version",
                version: StableSemver = {
                    value.version
                },
            );
        }
        struct VersionedCredentialMaterialV1
            from (crate::VersionedCredentialReference)
            using project_versioned_credential_material(value) {
            (
                "VersionedCredentialReference.credential",
                "VersionedCredentialReference.credential",
                Model,
                "VersionedCredentialMaterialV1.credential",
                "normalized",
                "scalar",
                "required",
                "CredentialSpecId",
                "VersionedCredentialMaterialV1.credential",
                credential: String = {
                    value.credential.as_str().to_owned()
                },
            );
            (
                "VersionedCredentialReference.version",
                "VersionedCredentialReference.version",
                Model,
                "VersionedCredentialMaterialV1.version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "VersionedCredentialMaterialV1.version",
                version: StableSemver = {
                    value.version
                },
            );
        }
        struct ResponseMappingMaterialV1
            from (crate::ResponseMapping)
            using project_response_mapping_material(value) {
            (
                "ResponseMapping.pointer",
                "ResponseMapping.pointer",
                Model,
                "ResponseMappingMaterialV1.pointer",
                "normalized",
                "scalar",
                "required",
                "StaticJsonPointer",
                "ResponseMappingMaterialV1.pointer",
                pointer: String = {
                    value.pointer.clone()
                },
            );
            (
                "ResponseMapping.target",
                "ResponseMapping.target",
                Model,
                "ResponseMappingMaterialV1.target",
                "normalized",
                "scalar",
                "required",
                "Id",
                "ResponseMappingMaterialV1.target",
                target: String = {
                    value.target.clone()
                },
            );
        }
        struct SelectedResponseHeaderMaterialV1
            from (SelectedResponseHeader)
            using project_selected_header_material(value) {
            (
                "SelectedResponseHeader.canonical_lowercase_header_name",
                "SelectedResponseHeader.canonical_lowercase_header_name",
                Model,
                "SelectedResponseHeaderMaterialV1.canonical_lowercase_header_name",
                "normalized",
                "scalar",
                "required",
                "StaticHeaderName",
                "SelectedResponseHeaderMaterialV1.canonical_lowercase_header_name",
                canonical_lowercase_header_name: String = {
                    value.canonical_lowercase_header_name.clone()
                },
            );
            (
                "SelectedResponseHeader.capability",
                "SelectedResponseHeader.capability",
                Model,
                "SelectedResponseHeaderMaterialV1.capability",
                "normalized",
                "scalar",
                "required",
                "CapabilityId",
                "SelectedResponseHeaderMaterialV1.capability",
                capability: String = {
                    value.capability.as_str().to_owned()
                },
            );
        }
        struct StatusRangeMaterialV1
            from (crate::StatusRange)
            using project_status_range_material(value) {
            (
                "StatusRange.minimum",
                "StatusRange.minimum",
                Model,
                "StatusRangeMaterialV1.minimum",
                "normalized",
                "scalar",
                "required",
                "u16",
                "StatusRangeMaterialV1.minimum",
                minimum: u16 = {
                    value.minimum
                },
            );
            (
                "StatusRange.maximum",
                "StatusRange.maximum",
                Model,
                "StatusRangeMaterialV1.maximum",
                "normalized",
                "scalar",
                "required",
                "u16",
                "StatusRangeMaterialV1.maximum",
                maximum: u16 = {
                    value.maximum
                },
            );
        }
        struct StepBoundsMaterialV1
            from (crate::StepBounds)
            using project_step_bounds_material(value) {
            (
                "StepBounds.maximum_headers",
                "StepBounds.maximum_headers",
                Model,
                "StepBoundsMaterialV1.maximum_headers",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_headers",
                maximum_headers: u32 = {
                    value.maximum_headers.get()
                },
            );
            (
                "StepBounds.maximum_header_bytes",
                "StepBounds.maximum_header_bytes",
                Model,
                "StepBoundsMaterialV1.maximum_header_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_header_bytes",
                maximum_header_bytes: u32 = {
                    value.maximum_header_bytes.get()
                },
            );
            (
                "StepBounds.maximum_url_bytes",
                "StepBounds.maximum_url_bytes",
                Model,
                "StepBoundsMaterialV1.maximum_url_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_url_bytes",
                maximum_url_bytes: u32 = {
                    value.maximum_url_bytes.get()
                },
            );
            (
                "StepBounds.maximum_request_bytes",
                "StepBounds.maximum_request_bytes",
                Model,
                "StepBoundsMaterialV1.maximum_request_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_request_bytes",
                maximum_request_bytes: u32 = {
                    value.maximum_request_bytes.get()
                },
            );
            (
                "StepBounds.maximum_response_bytes",
                "StepBounds.maximum_response_bytes",
                Model,
                "StepBoundsMaterialV1.maximum_response_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_response_bytes",
                maximum_response_bytes: u32 = {
                    value.maximum_response_bytes.get()
                },
            );
            (
                "StepBounds.maximum_json_depth",
                "StepBounds.maximum_json_depth",
                Model,
                "StepBoundsMaterialV1.maximum_json_depth",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_json_depth",
                maximum_json_depth: u32 = {
                    value.maximum_json_depth.get()
                },
            );
            (
                "StepBounds.maximum_json_nodes",
                "StepBounds.maximum_json_nodes",
                Model,
                "StepBoundsMaterialV1.maximum_json_nodes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_json_nodes",
                maximum_json_nodes: u32 = {
                    value.maximum_json_nodes.get()
                },
            );
            (
                "StepBounds.maximum_inline_binary_bytes",
                "StepBounds.maximum_inline_binary_bytes",
                Model,
                "StepBoundsMaterialV1.maximum_inline_binary_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "StepBoundsMaterialV1.maximum_inline_binary_bytes",
                maximum_inline_binary_bytes: u32 = {
                    value.maximum_inline_binary_bytes.get()
                },
            );
            (
                "StepBounds.deadline_ms",
                "StepBounds.deadline_ms",
                Model,
                "StepBoundsMaterialV1.deadline_ms",
                "normalized",
                "scalar",
                "required",
                "u64-string",
                "StepBoundsMaterialV1.deadline_ms",
                deadline_ms: String = {
                    value.deadline_ms.get().to_string()
                },
            );
        }
        struct OperationBoundsMaterialV1
            from (crate::OperationBounds)
            using project_operation_bounds_material(value) {
            (
                "OperationBounds.maximum_calls",
                "OperationBounds.maximum_calls",
                Model,
                "OperationBoundsMaterialV1.maximum_calls",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_calls",
                maximum_calls: u32 = {
                    value.maximum_calls.get()
                },
            );
            (
                "OperationBounds.maximum_pages",
                "OperationBounds.maximum_pages",
                Model,
                "OperationBoundsMaterialV1.maximum_pages",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_pages",
                maximum_pages: u32 = {
                    value.maximum_pages.get()
                },
            );
            (
                "OperationBounds.maximum_items",
                "OperationBounds.maximum_items",
                Model,
                "OperationBoundsMaterialV1.maximum_items",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_items",
                maximum_items: u32 = {
                    value.maximum_items.get()
                },
            );
            (
                "OperationBounds.maximum_aggregate_request_bytes",
                "OperationBounds.maximum_aggregate_request_bytes",
                Model,
                "OperationBoundsMaterialV1.maximum_aggregate_request_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_aggregate_request_bytes",
                maximum_aggregate_request_bytes: u32 = {
                    value.maximum_aggregate_request_bytes.get()
                },
            );
            (
                "OperationBounds.maximum_aggregate_response_bytes",
                "OperationBounds.maximum_aggregate_response_bytes",
                Model,
                "OperationBoundsMaterialV1.maximum_aggregate_response_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_aggregate_response_bytes",
                maximum_aggregate_response_bytes: u32 = {
                    value.maximum_aggregate_response_bytes.get()
                },
            );
            (
                "OperationBounds.maximum_output_canonical_bytes",
                "OperationBounds.maximum_output_canonical_bytes",
                Model,
                "OperationBoundsMaterialV1.maximum_output_canonical_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "OperationBoundsMaterialV1.maximum_output_canonical_bytes",
                maximum_output_canonical_bytes: u32 = {
                    value.maximum_output_canonical_bytes.get()
                },
            );
            (
                "OperationBounds.maximum_redirects",
                "OperationBounds.maximum_redirects",
                Model,
                "OperationBoundsMaterialV1.maximum_redirects",
                "normalized",
                "scalar",
                "required",
                "u8",
                "OperationBoundsMaterialV1.maximum_redirects",
                maximum_redirects: u8 = {
                    value.maximum_redirects
                },
            );
            (
                "OperationBounds.deadline_ms",
                "OperationBounds.deadline_ms",
                Model,
                "OperationBoundsMaterialV1.deadline_ms",
                "normalized",
                "scalar",
                "required",
                "u64-string",
                "OperationBoundsMaterialV1.deadline_ms",
                deadline_ms: String = {
                    value.deadline_ms.get().to_string()
                },
            );
        }
        struct CapacityDefaultsMaterialV1
            from (crate::CapacityDefaults)
            using project_capacity_defaults_material(value) {
            (
                "CapacityDefaults.maximum_in_flight",
                "CapacityDefaults.maximum_in_flight",
                Model,
                "CapacityDefaultsMaterialV1.maximum_in_flight",
                "normalized",
                "scalar",
                "required",
                "u32",
                "CapacityDefaultsMaterialV1.maximum_in_flight",
                maximum_in_flight: u32 = {
                    value.maximum_in_flight.get()
                },
            );
        }
        struct RateDefaultsMaterialV1
            from (crate::RateDefaults)
            using project_rate_defaults_material(value) {
            (
                "RateDefaults.burst",
                "RateDefaults.burst",
                Model,
                "RateDefaultsMaterialV1.burst",
                "normalized",
                "scalar",
                "required",
                "u32",
                "RateDefaultsMaterialV1.burst",
                burst: u32 = {
                    value.burst.get()
                },
            );
            (
                "RateDefaults.refill_interval_ms",
                "RateDefaults.refill_interval_ms",
                Model,
                "RateDefaultsMaterialV1.refill_interval_ms",
                "normalized",
                "scalar",
                "required",
                "u64-string",
                "RateDefaultsMaterialV1.refill_interval_ms",
                refill_interval_ms: String = {
                    value.refill_interval_ms.get().to_string()
                },
            );
        }
        struct TypedSerializationKeyDefaultMaterialV1
            from (crate::TypedSerializationKeyDefault)
            using project_serialization_key_default_material(value) {
            (
                "TypedSerializationKeyDefault.field",
                "TypedSerializationKeyDefault.field",
                Model,
                "TypedSerializationKeyDefaultMaterialV1.field",
                "normalized",
                "scalar",
                "required",
                "Id",
                "TypedSerializationKeyDefaultMaterialV1.field",
                field: String = {
                    value.field.clone()
                },
            );
            (
                "TypedSerializationKeyDefault.value",
                "TypedSerializationKeyDefault.value",
                Model,
                "TypedSerializationKeyDefaultMaterialV1.value",
                "normalized",
                "scalar",
                "required",
                "TypedValueMaterialV1",
                "TypedSerializationKeyDefaultMaterialV1.value",
                #[serde(
                    deserialize_with =
                        "crate::source::deserialize_typed_value_material"
                )]
                value: TypedValueMaterialV1 = {
                    typed_value_material(&value.value)
                },
            );
        }
        struct PaginationBoundsMaterialV1
            from (crate::PaginationBounds)
            using project_pagination_bounds_material(value) {
            (
                "PaginationBounds.maximum_calls",
                "PaginationBounds.maximum_calls",
                Model,
                "PaginationBoundsMaterialV1.maximum_calls",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_calls",
                maximum_calls: u32 = {
                    value.maximum_calls.get()
                },
            );
            (
                "PaginationBounds.maximum_pages",
                "PaginationBounds.maximum_pages",
                Model,
                "PaginationBoundsMaterialV1.maximum_pages",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_pages",
                maximum_pages: u32 = {
                    value.maximum_pages.get()
                },
            );
            (
                "PaginationBounds.maximum_items",
                "PaginationBounds.maximum_items",
                Model,
                "PaginationBoundsMaterialV1.maximum_items",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_items",
                maximum_items: u32 = {
                    value.maximum_items.get()
                },
            );
            (
                "PaginationBounds.maximum_response_bytes",
                "PaginationBounds.maximum_response_bytes",
                Model,
                "PaginationBoundsMaterialV1.maximum_response_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_response_bytes",
                maximum_response_bytes: u32 = {
                    value.maximum_response_bytes.get()
                },
            );
            (
                "PaginationBounds.maximum_aggregate_response_bytes",
                "PaginationBounds.maximum_aggregate_response_bytes",
                Model,
                "PaginationBoundsMaterialV1.maximum_aggregate_response_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_aggregate_response_bytes",
                maximum_aggregate_response_bytes: u32 = {
                    value.maximum_aggregate_response_bytes.get()
                },
            );
            (
                "PaginationBounds.maximum_output_canonical_bytes",
                "PaginationBounds.maximum_output_canonical_bytes",
                Model,
                "PaginationBoundsMaterialV1.maximum_output_canonical_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "PaginationBoundsMaterialV1.maximum_output_canonical_bytes",
                maximum_output_canonical_bytes: u32 = {
                    value.maximum_output_canonical_bytes.get()
                },
            );
        }
        struct SubscriptionOperationIdsMaterialV1
            from (crate::SubscriptionOperationIds)
            using project_subscription_operation_ids_material(value) {
            (
                "SubscriptionOperationIds.create",
                "SubscriptionOperationIds.create",
                Model,
                "SubscriptionOperationIdsMaterialV1.create",
                "normalized",
                "scalar",
                "required",
                "OperationId",
                "SubscriptionOperationIdsMaterialV1.create",
                create: String = {
                    value.create.as_str().to_owned()
                },
            );
            (
                "SubscriptionOperationIds.delete",
                "SubscriptionOperationIds.delete",
                Model,
                "SubscriptionOperationIdsMaterialV1.delete",
                "normalized",
                "scalar",
                "required",
                "OperationId",
                "SubscriptionOperationIdsMaterialV1.delete",
                delete: String = {
                    value.delete.as_str().to_owned()
                },
            );
            (
                "SubscriptionOperationIds.check",
                "SubscriptionOperationIds.check",
                Model,
                "SubscriptionOperationIdsMaterialV1.check",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<OperationId>",
                "SubscriptionOperationIdsMaterialV1.check",
                check: Option<String> = {
                    value
                        .check
                        .as_ref()
                        .map(|operation| operation.as_str().to_owned())
                },
            );
        }
        struct SemanticCredentialMaterialV1
            from (crate::CredentialSpec)
            using semantic_credential_material(value) {
            (
                "CredentialSpec.credential",
                "CredentialSpec.credential",
                Model,
                "SemanticCredentialMaterialV1.credential",
                "normalized",
                "scalar",
                "required",
                "CredentialSpecId",
                "SemanticCredentialMaterialV1.credential",
                credential: String = {
                    value.credential.as_str().to_owned()
                },
            );
            (
                "CredentialSpec.version",
                "CredentialSpec.version",
                Model,
                "SemanticCredentialMaterialV1.version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "SemanticCredentialMaterialV1.version",
                version: StableSemver = {
                    value.version
                },
            );
            (
                "CredentialSpec.fields",
                "CredentialSpec.fields",
                Model,
                "SemanticCredentialMaterialV1.fields",
                "normalized",
                "field",
                "nonempty_array",
                "NonEmptyVec<CredentialFieldMaterialV1>",
                "SemanticCredentialMaterialV1.fields",
                fields: Vec<CredentialFieldMaterialV1> = {
                    let mut fields = value
                        .fields
                        .iter()
                        .map(credential_field_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    fields.sort_by(|left, right| {
                        left.field.cmp(&right.field)
                    });
                    fields
                },
            );
            (
                "CredentialSpec.auth_plan",
                "CredentialSpec.auth_plan",
                Model,
                "SemanticCredentialMaterialV1.auth_plan",
                "normalized",
                "scalar",
                "required",
                "CredentialAuthMaterialV1",
                "SemanticCredentialMaterialV1.auth_plan",
                auth_plan: CredentialAuthMaterialV1 = {
                    credential_auth_material(&value.auth_plan)?
                },
            );
            (
                "CredentialSpec.allowed_origins",
                "CredentialSpec.allowed_origins",
                Model,
                "SemanticCredentialMaterialV1.allowed_origins",
                "normalized",
                "lexical",
                "nonempty_array",
                "NonEmptyVec<OriginId>",
                "SemanticCredentialMaterialV1.allowed_origins",
                allowed_origins: Vec<String> = {
                    let mut origins = value
                        .allowed_origins
                        .iter()
                        .map(|origin| origin.as_str().to_owned())
                        .collect::<Vec<_>>();
                    origins.sort();
                    origins
                },
            );
            (
                "CredentialSpec.scopes",
                "CredentialSpec.scopes",
                Model,
                "SemanticCredentialMaterialV1.scopes",
                "normalized",
                "lexical",
                "empty_array",
                "Vec<StaticScope>",
                "SemanticCredentialMaterialV1.scopes",
                scopes: Vec<String> = {
                    let mut scopes = value.scopes.clone();
                    scopes.sort();
                    scopes
                },
            );
            (
                "CredentialSpec.auth_processor",
                "CredentialSpec.auth_processor",
                Model,
                "SemanticCredentialMaterialV1.auth_processor",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<VersionedProcessorRef>",
                "SemanticCredentialMaterialV1.auth_processor",
                auth_processor: Option<VersionedProcessorMaterialV1> = {
                    value
                        .auth_processor
                        .as_ref()
                        .map(project_versioned_processor_material)
                        .transpose()?
                },
            );
            (
                "CredentialSpec.credential_test_operation",
                "CredentialSpec.credential_test_operation",
                Model,
                "SemanticCredentialMaterialV1.credential_test_operation",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<VersionedOperationReference>",
                "SemanticCredentialMaterialV1.credential_test_operation",
                credential_test_operation:
                    Option<VersionedOperationReferenceMaterialV1> = {
                    value
                        .credential_test_operation
                        .as_ref()
                        .map(project_versioned_operation_reference_material)
                        .transpose()?
                },
            );
            (
                "CredentialSpec.bounds",
                "CredentialSpec.bounds",
                Model,
                "SemanticCredentialMaterialV1.bounds",
                "normalized",
                "scalar",
                "required",
                "CredentialBoundsMaterialV1",
                "SemanticCredentialMaterialV1.bounds",
                bounds: CredentialBoundsMaterialV1 = {
                    project_credential_bounds_material(&value.bounds)?
                },
            );
        }
        struct CredentialFieldMaterialV1
            from (crate::CredentialFieldSpec)
            using credential_field_material(value) {
            (
                "CredentialFieldSpec.field",
                "CredentialFieldSpec.field",
                Model,
                "CredentialFieldMaterialV1.field",
                "normalized",
                "scalar",
                "required",
                "CredentialFieldId",
                "CredentialFieldMaterialV1.field",
                field: String = {
                    value.field.as_str().to_owned()
                },
            );
            (
                "CredentialFieldSpec.required",
                "CredentialFieldSpec.required",
                Model,
                "CredentialFieldMaterialV1.required",
                "normalized",
                "scalar",
                "required",
                "bool",
                "CredentialFieldMaterialV1.required",
                required: bool = {
                    value.required
                },
            );
            (
                "CredentialFieldSpec.secret",
                "CredentialFieldSpec.secret",
                Model,
                "CredentialFieldMaterialV1.secret",
                "normalized",
                "scalar",
                "required",
                "SecretClassificationMaterialV1",
                "CredentialFieldMaterialV1.secret",
                secret: SecretClassificationMaterialV1 = {
                    secret_material(&value.secret)?
                },
            );
            (
                "CredentialFieldSpec.maximum_bytes",
                "CredentialFieldSpec.maximum_bytes",
                Model,
                "CredentialFieldMaterialV1.maximum_bytes",
                "normalized",
                "scalar",
                "required",
                "u32",
                "CredentialFieldMaterialV1.maximum_bytes",
                maximum_bytes: u32 = {
                    value.maximum_bytes.get()
                },
            );
            (
                "CredentialFieldSpec.redaction",
                "CredentialFieldSpec.redaction",
                Model,
                "CredentialFieldMaterialV1.redaction",
                "normalized",
                "scalar",
                "required",
                "RedactionMaterialV1",
                "CredentialFieldMaterialV1.redaction",
                redaction: RedactionMaterialV1 = {
                    redaction_material(&value.redaction)?
                },
            );
        }
        struct SemanticOriginMaterialV1
            from (crate::FixedOrigin)
            using semantic_origin_material(value) {
            (
                "FixedOrigin.origin",
                "FixedOrigin.origin",
                Model,
                "SemanticOriginMaterialV1.origin",
                "normalized",
                "scalar",
                "required",
                "OriginId",
                "SemanticOriginMaterialV1.origin",
                origin: String = {
                    value.origin.as_str().to_owned()
                },
            );
            (
                "FixedOrigin.scheme",
                "FixedOrigin.scheme",
                Model,
                "SemanticOriginMaterialV1.scheme",
                "normalized",
                "scalar",
                "required",
                "HttpsOnly",
                "SemanticOriginMaterialV1.scheme",
                scheme: HttpsMaterialV1 = {
                    https_material(&value.scheme)?
                }, Singleton,
            );
            (
                "FixedOrigin.host",
                "FixedOrigin.host",
                Model,
                "SemanticOriginMaterialV1.host",
                "normalized",
                "scalar",
                "required",
                "StaticDnsName",
                "SemanticOriginMaterialV1.host",
                host: String = {
                    value.host.clone()
                },
            );
            (
                "FixedOrigin.port",
                "FixedOrigin.port",
                Model,
                "SemanticOriginMaterialV1.port",
                "normalized",
                "scalar",
                "required",
                "u16",
                "SemanticOriginMaterialV1.port",
                port: u16 = {
                    value.port.get()
                },
            );
            (
                "FixedOrigin.network_policy",
                "FixedOrigin.network_policy",
                Model,
                "SemanticOriginMaterialV1.network_policy",
                "normalized",
                "scalar",
                "required",
                "NetworkPolicyMaterialV1",
                "SemanticOriginMaterialV1.network_policy",
                network_policy: NetworkPolicyMaterialV1 = {
                    network_policy_material(&value.network_policy)?
                },
            );
        }
        struct SemanticOperationMaterialV1
            from (crate::OperationSpec)
            using semantic_operation_material(
                value;
                manifest_value_language_epoch: u32 = {
                    let operation_value_language_epoch =
                        validate_semantic_value_language_epoch(
                            value.value_language_epoch,
                        )?;
                    if operation_value_language_epoch
                        != manifest_value_language_epoch
                    {
                        return Err(CatalogError::new(
                            "catalog_projection_input_mismatch",
                            "operation value-language epoch must match manifest",
                        ));
                    }
                    operation_value_language_epoch
                }
            ) {
            (
                "OperationSpec.connector",
                "OperationSpec.connector",
                Model,
                "SemanticOperationMaterialV1.connector",
                "normalized",
                "scalar",
                "required",
                "ConnectorId",
                "SemanticOperationMaterialV1.connector",
                connector: String = {
                    value.connector.as_str().to_owned()
                },
            );
            (
                "OperationSpec.connector_version",
                "OperationSpec.connector_version",
                Model,
                "SemanticOperationMaterialV1.connector_version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "SemanticOperationMaterialV1.connector_version",
                connector_version: StableSemver = {
                    value.connector_version
                },
            );
            (
                "OperationSpec.operation",
                "OperationSpec.operation",
                Model,
                "SemanticOperationMaterialV1.operation",
                "normalized",
                "scalar",
                "required",
                "OperationId",
                "SemanticOperationMaterialV1.operation",
                operation: String = {
                    value.operation.as_str().to_owned()
                },
            );
            (
                "OperationSpec.operation_version",
                "OperationSpec.operation_version",
                Model,
                "SemanticOperationMaterialV1.operation_version",
                "normalized",
                "scalar",
                "required",
                "StableSemver",
                "SemanticOperationMaterialV1.operation_version",
                operation_version: StableSemver = {
                    value.operation_version
                },
            );
            (
                "OperationSpec.runtime_abi_epoch",
                "OperationSpec.runtime_abi_epoch",
                Model,
                "SemanticOperationMaterialV1.runtime_abi_epoch",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "SemanticOperationMaterialV1.runtime_abi_epoch",
                runtime_abi_epoch: u32 = {
                    value.runtime_abi_epoch
                },
            );
            (
                "OperationSpec.value_language_epoch",
                "OperationSpec.value_language_epoch",
                Model,
                "SemanticOperationMaterialV1.value_language_epoch",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "SemanticOperationMaterialV1.value_language_epoch",
                value_language_epoch: u32 = {
                    manifest_value_language_epoch
                },
            );
            (
                "OperationSpec.input",
                "OperationSpec.input",
                Model,
                "SemanticOperationMaterialV1.input",
                "normalized",
                "scalar",
                "required",
                "ValueContractMaterialV1",
                "SemanticOperationMaterialV1.input",
                #[serde(
                    deserialize_with =
                        "deserialize_value_contract_material"
                )]
                input: ValueContractMaterialV1 = {
                    value_contract_material(
                        &value.input,
                        manifest_value_language_epoch,
                    )?
                },
            );
            (
                "OperationSpec.input_contract_sha256",
                "OperationSpec.input_contract_sha256",
                Model,
                "SemanticOperationMaterialV1.input_contract_sha256",
                "normalized",
                "scalar",
                "required",
                "Hash256",
                "SemanticOperationMaterialV1.input_contract_sha256",
                input_contract_sha256: String = {
                    hex_bytes(&value.input_contract_sha256)
                },
            );
            (
                "OperationSpec.output",
                "OperationSpec.output",
                Model,
                "SemanticOperationMaterialV1.output",
                "normalized",
                "scalar",
                "required",
                "ValueContractMaterialV1",
                "SemanticOperationMaterialV1.output",
                #[serde(
                    deserialize_with =
                        "deserialize_value_contract_material"
                )]
                output: ValueContractMaterialV1 = {
                    value_contract_material(
                        &value.output,
                        manifest_value_language_epoch,
                    )?
                },
            );
            (
                "OperationSpec.output_contract_sha256",
                "OperationSpec.output_contract_sha256",
                Model,
                "SemanticOperationMaterialV1.output_contract_sha256",
                "normalized",
                "scalar",
                "required",
                "Hash256",
                "SemanticOperationMaterialV1.output_contract_sha256",
                output_contract_sha256: String = {
                    hex_bytes(&value.output_contract_sha256)
                },
            );
            (
                "OperationSpec.credential",
                "OperationSpec.credential",
                Model,
                "SemanticOperationMaterialV1.credential",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<VersionedCredentialMaterialV1>",
                "SemanticOperationMaterialV1.credential",
                credential: Option<VersionedCredentialMaterialV1> = {
                    value
                        .credential
                        .as_ref()
                        .map(project_versioned_credential_material)
                        .transpose()?
                },
            );
            (
                "OperationSpec.origins",
                "OperationSpec.origins",
                Model,
                "SemanticOperationMaterialV1.origins",
                "normalized",
                "origin",
                "nonempty_array",
                "NonEmptyVec<SemanticOriginMaterialV1>",
                "SemanticOperationMaterialV1.origins",
                origins: Vec<SemanticOriginMaterialV1> = {
                    let mut origins = value
                        .origins
                        .iter()
                        .map(semantic_origin_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    origins.sort_by(|left, right| {
                        left.origin.cmp(&right.origin)
                    });
                    origins
                },
            );
            (
                "OperationSpec.steps",
                "OperationSpec.steps",
                Model,
                "SemanticOperationMaterialV1.steps",
                "normalized",
                "declared",
                "nonempty_array",
                "NonEmptyVec<SemanticStepMaterialV1>",
                "SemanticOperationMaterialV1.steps",
                steps: Vec<SemanticStepMaterialV1> = {
                    value
                        .steps
                        .iter()
                        .map(semantic_step_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?
                },
            );
            (
                "OperationSpec.pre_request_transforms",
                "OperationSpec.pre_request_transforms",
                Model,
                "SemanticOperationMaterialV1.pre_request_transforms",
                "normalized",
                "declared",
                "empty_array",
                "Vec<VersionedProcessorMaterialV1>",
                "SemanticOperationMaterialV1.pre_request_transforms",
                pre_request_transforms:
                    Vec<VersionedProcessorMaterialV1> = {
                    value
                        .pre_request_transforms
                        .iter()
                        .map(project_versioned_processor_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?
                },
            );
            (
                "OperationSpec.post_response_transforms",
                "OperationSpec.post_response_transforms",
                Model,
                "SemanticOperationMaterialV1.post_response_transforms",
                "normalized",
                "declared",
                "empty_array",
                "Vec<VersionedProcessorMaterialV1>",
                "SemanticOperationMaterialV1.post_response_transforms",
                post_response_transforms:
                    Vec<VersionedProcessorMaterialV1> = {
                    value
                        .post_response_transforms
                        .iter()
                        .map(project_versioned_processor_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?
                },
            );
            (
                "OperationSpec.operation_processor",
                "OperationSpec.operation_processor",
                Model,
                "SemanticOperationMaterialV1.operation_processor",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<VersionedProcessorMaterialV1>",
                "SemanticOperationMaterialV1.operation_processor",
                operation_processor:
                    Option<VersionedProcessorMaterialV1> = {
                    value
                        .operation_processor
                        .as_ref()
                        .map(project_versioned_processor_material)
                        .transpose()?
                },
            );
            (
                "OperationSpec.effect",
                "OperationSpec.effect",
                Model,
                "SemanticOperationMaterialV1.effect",
                "normalized",
                "scalar",
                "required",
                "OperationEffectMaterialV1",
                "SemanticOperationMaterialV1.effect",
                effect: OperationEffectMaterialV1 = {
                    operation_effect_material(&value.effect)?
                },
            );
            (
                "OperationSpec.pagination",
                "OperationSpec.pagination",
                Model,
                "SemanticOperationMaterialV1.pagination",
                "normalized",
                "scalar",
                "required",
                "PaginationMaterialV1",
                "SemanticOperationMaterialV1.pagination",
                pagination: PaginationMaterialV1 = {
                    pagination_material(&value.pagination)?
                },
            );
            (
                "OperationSpec.error_map",
                "OperationSpec.error_map",
                Model,
                "SemanticOperationMaterialV1.error_map",
                "normalized",
                "scalar",
                "required",
                "ErrorMapMaterialV1",
                "SemanticOperationMaterialV1.error_map",
                error_map: ErrorMapMaterialV1 = {
                    error_map_material(&value.error_map)?
                },
            );
            (
                "OperationSpec.capacity",
                "OperationSpec.capacity",
                Model,
                "SemanticOperationMaterialV1.capacity",
                "normalized",
                "scalar",
                "required",
                "CapacityDefaultsMaterialV1",
                "SemanticOperationMaterialV1.capacity",
                capacity: CapacityDefaultsMaterialV1 = {
                    project_capacity_defaults_material(&value.capacity)?
                },
            );
            (
                "OperationSpec.rate",
                "OperationSpec.rate",
                Model,
                "SemanticOperationMaterialV1.rate",
                "normalized",
                "scalar",
                "required",
                "RateDefaultsMaterialV1",
                "SemanticOperationMaterialV1.rate",
                rate: RateDefaultsMaterialV1 = {
                    project_rate_defaults_material(&value.rate)?
                },
            );
            (
                "OperationSpec.serialization_key_default",
                "OperationSpec.serialization_key_default",
                Model,
                "SemanticOperationMaterialV1.serialization_key_default",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<TypedSerializationKeyDefaultMaterialV1>",
                "SemanticOperationMaterialV1.serialization_key_default",
                serialization_key_default:
                    Option<TypedSerializationKeyDefaultMaterialV1> = {
                    value
                        .serialization_key_default
                        .as_ref()
                        .map(project_serialization_key_default_material)
                        .transpose()?
                },
            );
            (
                "OperationSpec.bounds",
                "OperationSpec.bounds",
                Model,
                "SemanticOperationMaterialV1.bounds",
                "normalized",
                "scalar",
                "required",
                "OperationBoundsMaterialV1",
                "SemanticOperationMaterialV1.bounds",
                bounds: OperationBoundsMaterialV1 = {
                    project_operation_bounds_material(&value.bounds)?
                },
            );
            (
                "OperationSpec.resolved_fact_values",
                "OperationSpec.resolved_fact_values",
                Model,
                "SemanticOperationMaterialV1.resolved_fact_values",
                "normalized",
                "use_site",
                "empty_array",
                "Vec<ResolvedFactValueMaterialV1>",
                "SemanticOperationMaterialV1.resolved_fact_values",
                #[serde(
                    deserialize_with =
                        "deserialize_resolved_fact_values"
                )]
                resolved_fact_values:
                    Vec<ResolvedFactValueMaterialV1> = {
                    let mut facts = value
                        .resolved_fact_values
                        .iter()
                        .map(project_resolved_fact_value_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    facts.sort_by(|left, right| {
                        left.use_site.cmp(&right.use_site)
                    });
                    facts
                },
            );
        }
        struct SemanticStepMaterialV1
            from (crate::CompiledStepSpec)
            using semantic_step_material(value) {
            (
                "CompiledStepSpec.step",
                "CompiledStepSpec.step",
                Model,
                "SemanticStepMaterialV1.step",
                "normalized",
                "scalar",
                "required",
                "CompiledStepId",
                "SemanticStepMaterialV1.step",
                step: String = {
                    value.step.as_str().to_owned()
                },
            );
            (
                "CompiledStepSpec.method",
                "CompiledStepSpec.method",
                Model,
                "SemanticStepMaterialV1.method",
                "normalized",
                "scalar",
                "required",
                "StaticHttpMethod",
                "SemanticStepMaterialV1.method",
                method: String = {
                    value.method.clone()
                },
            );
            (
                "CompiledStepSpec.origin",
                "CompiledStepSpec.origin",
                Model,
                "SemanticStepMaterialV1.origin",
                "normalized",
                "scalar",
                "required",
                "OriginId",
                "SemanticStepMaterialV1.origin",
                origin: String = {
                    value.origin.as_str().to_owned()
                },
            );
            (
                "CompiledStepSpec.path",
                "CompiledStepSpec.path",
                Model,
                "SemanticStepMaterialV1.path",
                "normalized",
                "scalar",
                "required",
                "StaticPathTemplate",
                "SemanticStepMaterialV1.path",
                path: String = {
                    value.path.clone()
                },
            );
            (
                "CompiledStepSpec.query",
                "CompiledStepSpec.query",
                Model,
                "SemanticStepMaterialV1.query",
                "normalized",
                "name",
                "empty_array",
                "Vec<CompiledQueryBindingMaterialV1>",
                "SemanticStepMaterialV1.query",
                query: Vec<CompiledQueryBindingMaterialV1> = {
                    let mut query = value
                        .query
                        .iter()
                        .map(project_query_binding_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    query.sort_by(|left, right| {
                        left.name.cmp(&right.name)
                    });
                    query
                },
            );
            (
                "CompiledStepSpec.headers",
                "CompiledStepSpec.headers",
                Model,
                "SemanticStepMaterialV1.headers",
                "normalized",
                "name",
                "empty_array",
                "Vec<CompiledHeaderBindingMaterialV1>",
                "SemanticStepMaterialV1.headers",
                headers: Vec<CompiledHeaderBindingMaterialV1> = {
                    let mut headers = value
                        .headers
                        .iter()
                        .map(project_header_binding_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    headers.sort_by(|left, right| {
                        left.name.cmp(&right.name)
                    });
                    headers
                },
            );
            (
                "CompiledStepSpec.credential_action",
                "CompiledStepSpec.credential_action",
                Model,
                "SemanticStepMaterialV1.credential_action",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<CompiledCredentialActionMaterialV1>",
                "SemanticStepMaterialV1.credential_action",
                credential_action:
                    Option<CompiledCredentialActionMaterialV1> = {
                    value
                        .credential_action
                        .as_ref()
                        .map(project_credential_action_material)
                        .transpose()?
                },
            );
            (
                "CompiledStepSpec.request",
                "CompiledStepSpec.request",
                Model,
                "SemanticStepMaterialV1.request",
                "normalized",
                "scalar",
                "required",
                "CompiledRequestMaterialV1",
                "SemanticStepMaterialV1.request",
                request: CompiledRequestMaterialV1 = {
                    request_shape_material(&value.request)?
                },
            );
            (
                "CompiledStepSpec.success_statuses",
                "CompiledStepSpec.success_statuses",
                Model,
                "SemanticStepMaterialV1.success_statuses",
                "normalized",
                "minimum_then_maximum",
                "nonempty_array",
                "NonEmptyVec<StatusRangeMaterialV1>",
                "SemanticStepMaterialV1.success_statuses",
                success_statuses: Vec<StatusRangeMaterialV1> = {
                    let mut statuses = value
                        .success_statuses
                        .iter()
                        .map(project_status_range_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    statuses.sort_by_key(|status| {
                        (status.minimum, status.maximum)
                    });
                    statuses
                },
            );
            (
                "CompiledStepSpec.response",
                "CompiledStepSpec.response",
                Model,
                "SemanticStepMaterialV1.response",
                "normalized",
                "scalar",
                "required",
                "CompiledResponseMaterialV1",
                "SemanticStepMaterialV1.response",
                response: CompiledResponseMaterialV1 = {
                    response_shape_material(&value.response)?
                },
            );
            (
                "CompiledStepSpec.selected_response_headers",
                "CompiledStepSpec.selected_response_headers",
                Model,
                "SemanticStepMaterialV1.selected_response_headers",
                "normalized",
                "canonical_lowercase_header_name",
                "empty_array",
                "Vec<SelectedResponseHeaderMaterialV1>",
                "SemanticStepMaterialV1.selected_response_headers",
                selected_response_headers:
                    Vec<SelectedResponseHeaderMaterialV1> = {
                    let mut headers = value
                        .selected_response_headers
                        .iter()
                        .map(project_selected_header_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    headers.sort_by(|left, right| {
                        left.canonical_lowercase_header_name.cmp(
                            &right.canonical_lowercase_header_name,
                        )
                    });
                    headers
                },
            );
            (
                "CompiledStepSpec.bounds",
                "CompiledStepSpec.bounds",
                Model,
                "SemanticStepMaterialV1.bounds",
                "normalized",
                "scalar",
                "required",
                "StepBoundsMaterialV1",
                "SemanticStepMaterialV1.bounds",
                bounds: StepBoundsMaterialV1 = {
                    project_step_bounds_material(&value.bounds)?
                },
            );
        }
        struct CompiledQueryBindingMaterialV1
            from (crate::CompiledQueryBinding)
            using project_query_binding_material(value) {
            (
                "CompiledQueryBinding.name",
                "CompiledQueryBinding.name",
                Model,
                "CompiledQueryBindingMaterialV1.name",
                "normalized",
                "scalar",
                "required",
                "StaticQueryKey",
                "CompiledQueryBindingMaterialV1.name",
                name: String = {
                    value.name.clone()
                },
            );
            (
                "CompiledQueryBinding.binding",
                "CompiledQueryBinding.binding",
                Model,
                "CompiledQueryBindingMaterialV1.binding",
                "normalized",
                "scalar",
                "required",
                "BindingMaterialV1",
                "CompiledQueryBindingMaterialV1.binding",
                binding: BindingMaterialV1 = {
                    compiled_binding_material(&value.binding)?
                },
            );
        }
        struct CompiledHeaderBindingMaterialV1
            from (crate::CompiledHeaderBinding)
            using project_header_binding_material(value) {
            (
                "CompiledHeaderBinding.name",
                "CompiledHeaderBinding.name",
                Model,
                "CompiledHeaderBindingMaterialV1.name",
                "normalized",
                "scalar",
                "required",
                "StaticHeaderName",
                "CompiledHeaderBindingMaterialV1.name",
                name: String = {
                    value.name.clone()
                },
            );
            (
                "CompiledHeaderBinding.binding",
                "CompiledHeaderBinding.binding",
                Model,
                "CompiledHeaderBindingMaterialV1.binding",
                "normalized",
                "scalar",
                "required",
                "BindingMaterialV1",
                "CompiledHeaderBindingMaterialV1.binding",
                binding: BindingMaterialV1 = {
                    compiled_binding_material(&value.binding)?
                },
            );
        }
        struct BindingMaterialV1
            from (crate::CompiledBinding)
            using compiled_binding_material(value) {
            (
                "CompiledBinding.field",
                "CompiledBinding.field",
                Model,
                "BindingMaterialV1.field",
                "normalized",
                "scalar",
                "required",
                "Id",
                "BindingMaterialV1.field",
                field: String = {
                    value.field.clone()
                },
            );
            (
                "CompiledBinding.source",
                "CompiledBinding.source",
                Model,
                "BindingMaterialV1.source",
                "normalized",
                "scalar",
                "required",
                "CompiledBindingSourceMaterialV1",
                "BindingMaterialV1.source",
                source: CompiledBindingSourceMaterialV1 = {
                    compiled_binding_source_material(&value.source)?
                },
            );
            (
                "CompiledBinding.required",
                "CompiledBinding.required",
                Model,
                "BindingMaterialV1.required",
                "normalized",
                "scalar",
                "required",
                "bool",
                "BindingMaterialV1.required",
                required: bool = {
                    value.required
                },
            );
            (
                "CompiledBinding.default",
                "CompiledBinding.default",
                Model,
                "BindingMaterialV1.default",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<TypedValueMaterialV1>",
                "BindingMaterialV1.default",
                #[serde(
                    deserialize_with =
                        "crate::source::deserialize_optional_typed_value_material"
                )]
                default: Option<TypedValueMaterialV1> = {
                    value.default.as_ref().map(typed_value_material)
                },
            );
            (
                "CompiledBinding.mapping",
                "CompiledBinding.mapping",
                Model,
                "BindingMaterialV1.mapping",
                "normalized",
                "scalar",
                "explicit_null",
                "Option<Id>",
                "BindingMaterialV1.mapping",
                mapping: Option<String> = {
                    value.mapping.clone()
                },
            );
        }
        struct CompiledCredentialActionMaterialV1
            from (crate::CompiledCredentialAction)
            using project_credential_action_material(value) {
            (
                "CompiledCredentialAction.credential",
                "CompiledCredentialAction.credential",
                Model,
                "CompiledCredentialActionMaterialV1.credential",
                "normalized",
                "scalar",
                "required",
                "CredentialSpecId",
                "CompiledCredentialActionMaterialV1.credential",
                credential: String = {
                    value.credential.as_str().to_owned()
                },
            );
        }
        struct ProviderIdempotentStepMaterialV1
            from (crate::ProviderIdempotentStep)
            using project_provider_idempotent_step_material(value) {
            (
                "ProviderIdempotentStep.step",
                "ProviderIdempotentStep.step",
                Model,
                "ProviderIdempotentStepMaterialV1.step",
                "normalized",
                "scalar",
                "required",
                "CompiledStepId",
                "ProviderIdempotentStepMaterialV1.step",
                step: String = {
                    value.step.as_str().to_owned()
                },
            );
            (
                "ProviderIdempotentStep.fixed_binding",
                "ProviderIdempotentStep.fixed_binding",
                Model,
                "ProviderIdempotentStepMaterialV1.fixed_binding",
                "normalized",
                "scalar",
                "required",
                "FixedIdempotencyBindingMaterialV1",
                "ProviderIdempotentStepMaterialV1.fixed_binding",
                fixed_binding: FixedIdempotencyBindingMaterialV1 = {
                    fixed_idempotency_binding_material(
                        &value.fixed_binding,
                    )?
                },
            );
            (
                "ProviderIdempotentStep.scope",
                "ProviderIdempotentStep.scope",
                Model,
                "ProviderIdempotentStepMaterialV1.scope",
                "normalized",
                "scalar",
                "required",
                "ProviderIdempotencyScope",
                "ProviderIdempotentStepMaterialV1.scope",
                scope: String = {
                    value.scope.clone()
                },
            );
            (
                "ProviderIdempotentStep.minimum_retention_ms",
                "ProviderIdempotentStep.minimum_retention_ms",
                Model,
                "ProviderIdempotentStepMaterialV1.minimum_retention_ms",
                "normalized",
                "scalar",
                "required",
                "u64-string",
                "ProviderIdempotentStepMaterialV1.minimum_retention_ms",
                minimum_retention_ms: String = {
                    value.minimum_retention_ms.get().to_string()
                },
            );
            (
                "ProviderIdempotentStep.clock_safety_margin_ms",
                "ProviderIdempotentStep.clock_safety_margin_ms",
                Model,
                "ProviderIdempotentStepMaterialV1.clock_safety_margin_ms",
                "normalized",
                "scalar",
                "required",
                "u64-string",
                "ProviderIdempotentStepMaterialV1.clock_safety_margin_ms",
                clock_safety_margin_ms: String = {
                    value.clock_safety_margin_ms.get().to_string()
                },
            );
        }
        struct ErrorMapMaterialV1
            from (crate::ErrorMap)
            using error_map_material(value) {
            (
                "ErrorMap.rules",
                "ErrorMap.rules",
                Model,
                "ErrorMapMaterialV1.rules",
                "normalized",
                "declared",
                "empty_array",
                "Vec<ErrorRuleMaterialV1>",
                "ErrorMapMaterialV1.rules",
                rules: Vec<ErrorRuleMaterialV1> = {
                    value
                        .rules
                        .iter()
                        .map(project_error_rule_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?
                },
            );
            (
                "ErrorMap.fallback",
                "ErrorMap.fallback",
                Model,
                "ErrorMapMaterialV1.fallback",
                "normalized",
                "scalar",
                "required",
                "CompleteErrorFallbackMaterialV1",
                "ErrorMapMaterialV1.fallback",
                fallback: CompleteErrorFallbackMaterialV1 = {
                    project_complete_error_fallback_material(
                        &value.fallback,
                    )?
                },
            );
        }
        struct ErrorRuleMaterialV1
            from (crate::ErrorRule)
            using project_error_rule_material(value) {
            (
                "ErrorRule.matcher",
                "ErrorRule.matcher",
                Model,
                "ErrorRuleMaterialV1.matcher",
                "normalized",
                "scalar",
                "required",
                "ErrorMatcherMaterialV1",
                "ErrorRuleMaterialV1.matcher",
                matcher: ErrorMatcherMaterialV1 = {
                    error_matcher_material(&value.matcher)?
                },
            );
            (
                "ErrorRule.action",
                "ErrorRule.action",
                Model,
                "ErrorRuleMaterialV1.action",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "ErrorRuleMaterialV1.action",
                action: ErrorActionMaterialV1 = {
                    error_action_material(&value.action)?
                },
            );
        }
        struct ErrorActionMaterialV1
            from (crate::ErrorAction)
            using error_action_material(value) {
            (
                "ErrorAction.class",
                "ErrorAction.class",
                Model,
                "ErrorActionMaterialV1.class",
                "normalized",
                "scalar",
                "required",
                "ConnectorErrorClassMaterialV1",
                "ErrorActionMaterialV1.class",
                class: ConnectorErrorClassMaterialV1 = {
                    connector_error_class_material(&value.class)?
                },
            );
            (
                "ErrorAction.code",
                "ErrorAction.code",
                Model,
                "ErrorActionMaterialV1.code",
                "normalized",
                "scalar",
                "required",
                "StaticErrorCode",
                "ErrorActionMaterialV1.code",
                code: String = {
                    value.code.as_str().to_owned()
                },
            );
            (
                "ErrorAction.safe_message",
                "ErrorAction.safe_message",
                Model,
                "ErrorActionMaterialV1.safe_message",
                "normalized",
                "scalar",
                "required",
                "StaticSafeMessage",
                "ErrorActionMaterialV1.safe_message",
                safe_message: String = {
                    value.safe_message.as_str().to_owned()
                },
            );
            (
                "ErrorAction.retry_after",
                "ErrorAction.retry_after",
                Model,
                "ErrorActionMaterialV1.retry_after",
                "normalized",
                "scalar",
                "required",
                "RetryAfterMaterialV1",
                "ErrorActionMaterialV1.retry_after",
                retry_after: RetryAfterMaterialV1 = {
                    retry_after_material(&value.retry_after)?
                },
            );
            (
                "ErrorAction.correlations",
                "ErrorAction.correlations",
                Model,
                "ErrorActionMaterialV1.correlations",
                "normalized",
                "step_then_header",
                "empty_array",
                "Vec<ErrorCorrelationMaterialV1>",
                "ErrorActionMaterialV1.correlations",
                correlations: Vec<ErrorCorrelationMaterialV1> = {
                    let mut correlations = value
                        .correlations
                        .iter()
                        .map(project_error_correlation_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    correlations.sort_by(|left, right| {
                        (
                            &left.step,
                            &left.canonical_lowercase_header_name,
                            &left.capability,
                        )
                            .cmp(&(
                                &right.step,
                                &right.canonical_lowercase_header_name,
                                &right.capability,
                            ))
                    });
                    correlations
                },
            );
        }
        struct ErrorCorrelationMaterialV1
            from (crate::ErrorCorrelationBinding)
            using project_error_correlation_material(value) {
            (
                "ErrorCorrelationBinding.canonical_lowercase_header_name",
                "ErrorCorrelationBinding.canonical_lowercase_header_name",
                Model,
                "ErrorCorrelationMaterialV1.canonical_lowercase_header_name",
                "normalized",
                "scalar",
                "required",
                "StaticHeaderName",
                "ErrorCorrelationMaterialV1.canonical_lowercase_header_name",
                canonical_lowercase_header_name: String = {
                    value.canonical_lowercase_header_name.clone()
                },
            );
            (
                "ErrorCorrelationBinding.capability",
                "ErrorCorrelationBinding.capability",
                Model,
                "ErrorCorrelationMaterialV1.capability",
                "normalized",
                "scalar",
                "required",
                "CapabilityId",
                "ErrorCorrelationMaterialV1.capability",
                capability: String = {
                    value.capability.as_str().to_owned()
                },
            );
            (
                "ErrorCorrelationBinding.step",
                "ErrorCorrelationBinding.step",
                Model,
                "ErrorCorrelationMaterialV1.step",
                "normalized",
                "scalar",
                "required",
                "CompiledStepId",
                "ErrorCorrelationMaterialV1.step",
                step: String = {
                    value.step.as_str().to_owned()
                },
            );
        }
        struct CompleteErrorFallbackMaterialV1
            from (crate::CompleteErrorFallback)
            using project_complete_error_fallback_material(value) {
            (
                "CompleteErrorFallback.transport",
                "CompleteErrorFallback.transport",
                Model,
                "CompleteErrorFallbackMaterialV1.transport",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.transport",
                transport: ErrorActionMaterialV1 = {
                    error_action_material(&value.transport)?
                },
            );
            (
                "CompleteErrorFallback.timeout",
                "CompleteErrorFallback.timeout",
                Model,
                "CompleteErrorFallbackMaterialV1.timeout",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.timeout",
                timeout: ErrorActionMaterialV1 = {
                    error_action_material(&value.timeout)?
                },
            );
            (
                "CompleteErrorFallback.http_429",
                "CompleteErrorFallback.http_429",
                Model,
                "CompleteErrorFallbackMaterialV1.http_429",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.http_429",
                http_429: ErrorActionMaterialV1 = {
                    error_action_material(&value.http_429)?
                },
            );
            (
                "CompleteErrorFallback.http_5xx",
                "CompleteErrorFallback.http_5xx",
                Model,
                "CompleteErrorFallbackMaterialV1.http_5xx",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.http_5xx",
                http_5xx: ErrorActionMaterialV1 = {
                    error_action_material(&value.http_5xx)?
                },
            );
            (
                "CompleteErrorFallback.authentication",
                "CompleteErrorFallback.authentication",
                Model,
                "CompleteErrorFallbackMaterialV1.authentication",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.authentication",
                authentication: ErrorActionMaterialV1 = {
                    error_action_material(&value.authentication)?
                },
            );
            (
                "CompleteErrorFallback.validation",
                "CompleteErrorFallback.validation",
                Model,
                "CompleteErrorFallbackMaterialV1.validation",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.validation",
                validation: ErrorActionMaterialV1 = {
                    error_action_material(&value.validation)?
                },
            );
            (
                "CompleteErrorFallback.permanent",
                "CompleteErrorFallback.permanent",
                Model,
                "CompleteErrorFallbackMaterialV1.permanent",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.permanent",
                permanent: ErrorActionMaterialV1 = {
                    error_action_material(&value.permanent)?
                },
            );
            (
                "CompleteErrorFallback.invariant",
                "CompleteErrorFallback.invariant",
                Model,
                "CompleteErrorFallbackMaterialV1.invariant",
                "normalized",
                "scalar",
                "required",
                "ErrorActionMaterialV1",
                "CompleteErrorFallbackMaterialV1.invariant",
                invariant: ErrorActionMaterialV1 = {
                    error_action_material(&value.invariant)?
                },
            );
        }
    }
    generic_structs {
        struct VersionedProcessorMaterialV1
            from (crate::VersionedProcessorRef<Id>)
            using project_versioned_processor_material<Id>(value)
            where Id: MaterialId
        {
            (
                "VersionedProcessorRef.id",
                "VersionedProcessorRef.id",
                Model,
                "VersionedProcessorMaterialV1.id",
                "normalized",
                "scalar",
                "required",
                "typed_processor_id",
                "VersionedProcessorMaterialV1.id",
                id: String = {
                    value.id.material_id().to_owned()
                },
            );
            (
                "VersionedProcessorRef.implementation_revision",
                "VersionedProcessorRef.implementation_revision",
                Model,
                "VersionedProcessorMaterialV1.implementation_revision",
                "normalized",
                "scalar",
                "required",
                "Epoch",
                "VersionedProcessorMaterialV1.implementation_revision",
                implementation_revision: u32 = {
                    value.implementation_revision
                },
            );
        }
    }
    closed_structs {
        pub struct ResolvedFactValueMaterialV1
            from (crate::ResolvedFactValue)
            using project_resolved_fact_value_material(value) {
            (
                "ResolvedFactValue.use_site",
                "ResolvedFactValue.use_site",
                Model,
                "ResolvedFactValueMaterialV1.use_site",
                "normalized",
                "scalar",
                "required",
                "Id",
                "ResolvedFactValueMaterialV1.use_site",
                use_site: String = {
                    value.use_site.clone()
                },
            );
            (
                "ResolvedFactValue.value",
                "ResolvedFactValue.value",
                Model,
                "ResolvedFactValueMaterialV1.value",
                "normalized",
                "scalar",
                "required",
                "TypedValueMaterialV1",
                "ResolvedFactValueMaterialV1.value",
                value: TypedValueMaterialV1 = {
                    typed_value_material(&value.value)
                },
            );
        }
    }
    tagged_enums {
        enum SecretClassificationMaterialV1
            from (crate::SecretClassification)
            using secret_material(value) {
            unit_variants {
                (
                    "SecretClassification::Secret",
                    "SecretClassification::Secret",
                    Model,
                    "SecretClassificationMaterialV1{kind=secret}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "secret",
                    "SecretClassificationMaterialV1::Secret",
                    Secret as "secret",
                );
                (
                    "SecretClassification::Sensitive",
                    "SecretClassification::Sensitive",
                    Model,
                    "SecretClassificationMaterialV1{kind=sensitive}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "sensitive",
                    "SecretClassificationMaterialV1::Sensitive",
                    Sensitive as "sensitive",
                );
                (
                    "SecretClassification::NonSecret",
                    "SecretClassification::NonSecret",
                    Model,
                    "SecretClassificationMaterialV1{kind=non_secret}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "non_secret",
                    "SecretClassificationMaterialV1::NonSecret",
                    NonSecret as "non_secret",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {}
        }
        enum RedactionMaterialV1
            from (crate::RedactionPlan)
            using redaction_material(value) {
            unit_variants {
                (
                    "RedactionPlan::Omit",
                    "RedactionPlan::Omit",
                    Model,
                    "RedactionMaterialV1{kind=omit}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "omit",
                    "RedactionMaterialV1::Omit",
                    Omit as "omit",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "RedactionPlan::Fixed",
                    "RedactionPlan::Fixed",
                    Model,
                    "RedactionMaterialV1{kind=fixed}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "fixed",
                    "RedactionMaterialV1::Fixed",
                    Fixed as "fixed" {
                        (
                            "RedactionPlan::Fixed.replacement",
                            "RedactionPlan::Fixed.replacement",
                            Model,
                            "RedactionMaterialV1{kind=fixed}.value.replacement",
                            "normalized",
                            "scalar",
                            "required",
                            "string",
                            "RedactionMaterialV1::Fixed.replacement",
                            replacement: String = {
                                replacement.clone()
                            },
                        );
                    },
                );
                (
                    "RedactionPlan::PreserveLast",
                    "RedactionPlan::PreserveLast",
                    Model,
                    "RedactionMaterialV1{kind=preserve_last}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "preserve_last",
                    "RedactionMaterialV1::PreserveLast",
                    PreserveLast as "preserve_last" {
                        (
                            "RedactionPlan::PreserveLast.characters",
                            "RedactionPlan::PreserveLast.characters",
                            Model,
                            "RedactionMaterialV1{kind=preserve_last}.value.characters",
                            "normalized",
                            "scalar",
                            "required",
                            "u8",
                            "RedactionMaterialV1::PreserveLast.characters",
                            characters: u8 = {
                                *characters
                            },
                        );
                    },
                );
            }
        }
        #[allow(clippy::large_enum_variant)]
        enum CredentialAuthMaterialV1
            from (crate::AuthPlan)
            using credential_auth_material(value) {
            unit_variants {}
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "AuthPlan::FixedHeaderApiKey",
                    "AuthPlan::FixedHeaderApiKey",
                    Model,
                    "CredentialAuthMaterialV1{kind=fixed_header_api_key}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "fixed_header_api_key",
                    "CredentialAuthMaterialV1::FixedHeaderApiKey",
                    FixedHeaderApiKey as "fixed_header_api_key" {
                        (
                            "AuthPlan::FixedHeaderApiKey.field",
                            "AuthPlan::FixedHeaderApiKey.field",
                            Model,
                            "CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.field",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::FixedHeaderApiKey.field",
                            field: String = {
                                field.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::FixedHeaderApiKey.header",
                            "AuthPlan::FixedHeaderApiKey.header",
                            Model,
                            "CredentialAuthMaterialV1{kind=fixed_header_api_key}.value.header",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticHeaderName",
                            "CredentialAuthMaterialV1::FixedHeaderApiKey.header",
                            header: String = {
                                header.clone()
                            },
                        );
                    },
                );
                (
                    "AuthPlan::FixedQueryApiKey",
                    "AuthPlan::FixedQueryApiKey",
                    Model,
                    "CredentialAuthMaterialV1{kind=fixed_query_api_key}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "fixed_query_api_key",
                    "CredentialAuthMaterialV1::FixedQueryApiKey",
                    FixedQueryApiKey as "fixed_query_api_key" {
                        (
                            "AuthPlan::FixedQueryApiKey.field",
                            "AuthPlan::FixedQueryApiKey.field",
                            Model,
                            "CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.field",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::FixedQueryApiKey.field",
                            field: String = {
                                field.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::FixedQueryApiKey.query",
                            "AuthPlan::FixedQueryApiKey.query",
                            Model,
                            "CredentialAuthMaterialV1{kind=fixed_query_api_key}.value.query",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticQueryKey",
                            "CredentialAuthMaterialV1::FixedQueryApiKey.query",
                            query: String = {
                                query.clone()
                            },
                        );
                    },
                );
                (
                    "AuthPlan::Bearer",
                    "AuthPlan::Bearer",
                    Model,
                    "CredentialAuthMaterialV1{kind=bearer}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "bearer",
                    "CredentialAuthMaterialV1::Bearer",
                    Bearer as "bearer" {
                        (
                            "AuthPlan::Bearer.token",
                            "AuthPlan::Bearer.token",
                            Model,
                            "CredentialAuthMaterialV1{kind=bearer}.value.token",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::Bearer.token",
                            token: String = {
                                token.as_str().to_owned()
                            },
                        );
                    },
                );
                (
                    "AuthPlan::HttpBasic",
                    "AuthPlan::HttpBasic",
                    Model,
                    "CredentialAuthMaterialV1{kind=http_basic}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "http_basic",
                    "CredentialAuthMaterialV1::HttpBasic",
                    HttpBasic as "http_basic" {
                        (
                            "AuthPlan::HttpBasic.username",
                            "AuthPlan::HttpBasic.username",
                            Model,
                            "CredentialAuthMaterialV1{kind=http_basic}.value.username",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::HttpBasic.username",
                            username: String = {
                                username.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::HttpBasic.password",
                            "AuthPlan::HttpBasic.password",
                            Model,
                            "CredentialAuthMaterialV1{kind=http_basic}.value.password",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::HttpBasic.password",
                            password: String = {
                                password.as_str().to_owned()
                            },
                        );
                    },
                );
                (
                    "AuthPlan::OAuth2ClientCredentials",
                    "AuthPlan::OAuth2ClientCredentials",
                    Model,
                    "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "oauth2_client_credentials",
                    "CredentialAuthMaterialV1::OAuth2ClientCredentials",
                    OAuth2ClientCredentials as "oauth2_client_credentials" {
                        (
                            "AuthPlan::OAuth2ClientCredentials.client_id",
                            "AuthPlan::OAuth2ClientCredentials.client_id",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_id",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.client_id",
                            client_id: String = {
                                client_id.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::OAuth2ClientCredentials.client_secret",
                            "AuthPlan::OAuth2ClientCredentials.client_secret",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.client_secret",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.client_secret",
                            client_secret: String = {
                                client_secret.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::OAuth2ClientCredentials.token_origin",
                            "AuthPlan::OAuth2ClientCredentials.token_origin",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_origin",
                            "normalized",
                            "scalar",
                            "required",
                            "OriginId",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_origin",
                            token_origin: String = {
                                token_origin.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::OAuth2ClientCredentials.token_step",
                            "AuthPlan::OAuth2ClientCredentials.token_step",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_step",
                            "normalized",
                            "scalar",
                            "required",
                            "CompiledStepId",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_step",
                            token_step: String = {
                                token_step.as_str().to_owned()
                            },
                        );
                        (
                            "AuthPlan::OAuth2ClientCredentials.scopes",
                            "AuthPlan::OAuth2ClientCredentials.scopes",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.scopes",
                            "normalized",
                            "lexical",
                            "empty_array",
                            "Vec<StaticScope>",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.scopes",
                            scopes: Vec<String> = {
                                let mut scopes = scopes.clone();
                                scopes.sort();
                                scopes
                            },
                        );
                        (
                            "AuthPlan::OAuth2ClientCredentials.token_pointer",
                            "AuthPlan::OAuth2ClientCredentials.token_pointer",
                            Model,
                            "CredentialAuthMaterialV1{kind=oauth2_client_credentials}.value.token_pointer",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticJsonPointer",
                            "CredentialAuthMaterialV1::OAuth2ClientCredentials.token_pointer",
                            token_pointer: String = {
                                token_pointer.clone()
                            },
                        );
                    },
                );
                (
                    "AuthPlan::PreprovisionedOAuthAccessToken",
                    "AuthPlan::PreprovisionedOAuthAccessToken",
                    Model,
                    "CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "preprovisioned_oauth_access_token",
                    "CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken",
                    PreprovisionedOAuthAccessToken as
                        "preprovisioned_oauth_access_token" {
                        (
                            "AuthPlan::PreprovisionedOAuthAccessToken.token",
                            "AuthPlan::PreprovisionedOAuthAccessToken.token",
                            Model,
                            "CredentialAuthMaterialV1{kind=preprovisioned_oauth_access_token}.value.token",
                            "normalized",
                            "scalar",
                            "required",
                            "CredentialFieldId",
                            "CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken.token",
                            token: String = {
                                token.as_str().to_owned()
                            },
                        );
                    },
                );
            }
        }
        enum CompiledBindingSourceMaterialV1
            from (crate::CompiledBindingSource)
            using compiled_binding_source_material(value) {
            unit_variants {
                (
                    "CompiledBindingSource::Input",
                    "CompiledBindingSource::Input",
                    Model,
                    "CompiledBindingSourceMaterialV1{kind=input}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "input",
                    "CompiledBindingSourceMaterialV1::Input",
                    Input as "input",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "CompiledBindingSource::Constant",
                    "CompiledBindingSource::Constant",
                    Model,
                    "CompiledBindingSourceMaterialV1{kind=constant}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "constant",
                    "CompiledBindingSourceMaterialV1::Constant",
                    Constant as "constant" {
                        (
                            "CompiledBindingSource::Constant.value",
                            "CompiledBindingSource::Constant.value",
                            Model,
                            "CompiledBindingSourceMaterialV1{kind=constant}.value.value",
                            "normalized",
                            "scalar",
                            "required",
                            "TypedValueMaterialV1",
                            "CompiledBindingSourceMaterialV1::Constant.value",
                            #[serde(
                                deserialize_with =
                                    "crate::source::deserialize_typed_value_material"
                            )]
                            value: TypedValueMaterialV1 = {
                                typed_value_material(value)
                            },
                        );
                    },
                );
            }
        }
        enum CompiledRequestMaterialV1
            from (crate::CompiledRequestShape)
            using request_shape_material(value) {
            unit_variants {
                (
                    "CompiledRequestShape::None",
                    "CompiledRequestShape::None",
                    Model,
                    "CompiledRequestMaterialV1{kind=none}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "none",
                    "CompiledRequestMaterialV1::None",
                    None as "none",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "CompiledRequestShape::Json",
                    "CompiledRequestShape::Json",
                    Model,
                    "CompiledRequestMaterialV1{kind=json}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "json",
                    "CompiledRequestMaterialV1::Json",
                    Json as "json" {
                        (
                            "CompiledRequestShape::Json.bindings",
                            "CompiledRequestShape::Json.bindings",
                            Model,
                            "CompiledRequestMaterialV1{kind=json}.value.bindings",
                            "normalized",
                            "declared",
                            "empty_array",
                            "Vec<Id>",
                            "CompiledRequestMaterialV1::Json.bindings",
                            bindings: Vec<String> = {
                                bindings.clone()
                            },
                        );
                    },
                );
                (
                    "CompiledRequestShape::FormUrlencoded",
                    "CompiledRequestShape::FormUrlencoded",
                    Model,
                    "CompiledRequestMaterialV1{kind=form_urlencoded}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "form_urlencoded",
                    "CompiledRequestMaterialV1::FormUrlencoded",
                    FormUrlencoded as "form_urlencoded" {
                        (
                            "CompiledRequestShape::FormUrlencoded.bindings",
                            "CompiledRequestShape::FormUrlencoded.bindings",
                            Model,
                            "CompiledRequestMaterialV1{kind=form_urlencoded}.value.bindings",
                            "normalized",
                            "declared",
                            "empty_array",
                            "Vec<Id>",
                            "CompiledRequestMaterialV1::FormUrlencoded.bindings",
                            bindings: Vec<String> = {
                                bindings.clone()
                            },
                        );
                    },
                );
                (
                    "CompiledRequestShape::Multipart",
                    "CompiledRequestShape::Multipart",
                    Model,
                    "CompiledRequestMaterialV1{kind=multipart}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "multipart",
                    "CompiledRequestMaterialV1::Multipart",
                    Multipart as "multipart" {
                        (
                            "CompiledRequestShape::Multipart.bindings",
                            "CompiledRequestShape::Multipart.bindings",
                            Model,
                            "CompiledRequestMaterialV1{kind=multipart}.value.bindings",
                            "normalized",
                            "declared",
                            "empty_array",
                            "Vec<Id>",
                            "CompiledRequestMaterialV1::Multipart.bindings",
                            bindings: Vec<String> = {
                                bindings.clone()
                            },
                        );
                    },
                );
                (
                    "CompiledRequestShape::RawBytes",
                    "CompiledRequestShape::RawBytes",
                    Model,
                    "CompiledRequestMaterialV1{kind=raw_bytes}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "raw_bytes",
                    "CompiledRequestMaterialV1::RawBytes",
                    RawBytes as "raw_bytes" {
                        (
                            "CompiledRequestShape::RawBytes.binding",
                            "CompiledRequestShape::RawBytes.binding",
                            Model,
                            "CompiledRequestMaterialV1{kind=raw_bytes}.value.binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "CompiledRequestMaterialV1::RawBytes.binding",
                            binding: String = {
                                binding.clone()
                            },
                        );
                    },
                );
            }
        }
        enum CompiledResponseMaterialV1
            from (crate::CompiledResponseShape)
            using response_shape_material(value) {
            unit_variants {}
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "CompiledResponseShape::Json",
                    "CompiledResponseShape::Json",
                    Model,
                    "CompiledResponseMaterialV1{kind=json}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "json",
                    "CompiledResponseMaterialV1::Json",
                    Json as "json" {
                        (
                            "CompiledResponseShape::Json.mappings",
                            "CompiledResponseShape::Json.mappings",
                            Model,
                            "CompiledResponseMaterialV1{kind=json}.value.mappings",
                            "normalized",
                            "declared",
                            "empty_array",
                            "Vec<ResponseMappingMaterialV1>",
                            "CompiledResponseMaterialV1::Json.mappings",
                            mappings: Vec<ResponseMappingMaterialV1> = {
                                mappings
                                    .iter()
                                    .map(project_response_mapping_material)
                                    .collect::<Result<
                                        Vec<_>,
                                        CatalogError,
                                    >>()?
                            },
                        );
                    },
                );
                (
                    "CompiledResponseShape::RawBytes",
                    "CompiledResponseShape::RawBytes",
                    Model,
                    "CompiledResponseMaterialV1{kind=raw_bytes}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "raw_bytes",
                    "CompiledResponseMaterialV1::RawBytes",
                    RawBytes as "raw_bytes" {
                        (
                            "CompiledResponseShape::RawBytes.target",
                            "CompiledResponseShape::RawBytes.target",
                            Model,
                            "CompiledResponseMaterialV1{kind=raw_bytes}.value.target",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "CompiledResponseMaterialV1::RawBytes.target",
                            target: String = {
                                target.clone()
                            },
                        );
                    },
                );
            }
        }
        enum FixedIdempotencyBindingMaterialV1
            from (crate::FixedIdempotencyBinding)
            using fixed_idempotency_binding_material(value) {
            unit_variants {}
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "FixedIdempotencyBinding::Header",
                    "FixedIdempotencyBinding::Header",
                    Model,
                    "FixedIdempotencyBindingMaterialV1{kind=header}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "header",
                    "FixedIdempotencyBindingMaterialV1::Header",
                    Header as "header" {
                        (
                            "FixedIdempotencyBinding::Header.name",
                            "FixedIdempotencyBinding::Header.name",
                            Model,
                            "FixedIdempotencyBindingMaterialV1{kind=header}.value.name",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticHeaderName",
                            "FixedIdempotencyBindingMaterialV1::Header.name",
                            name: String = {
                                name.clone()
                            },
                        );
                    },
                );
                (
                    "FixedIdempotencyBinding::BodyField",
                    "FixedIdempotencyBinding::BodyField",
                    Model,
                    "FixedIdempotencyBindingMaterialV1{kind=body_field}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "body_field",
                    "FixedIdempotencyBindingMaterialV1::BodyField",
                    BodyField as "body_field" {
                        (
                            "FixedIdempotencyBinding::BodyField.pointer",
                            "FixedIdempotencyBinding::BodyField.pointer",
                            Model,
                            "FixedIdempotencyBindingMaterialV1{kind=body_field}.value.pointer",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticBodyPointer",
                            "FixedIdempotencyBindingMaterialV1::BodyField.pointer",
                            pointer: String = {
                                pointer.clone()
                            },
                        );
                    },
                );
            }
        }
        enum OperationEffectMaterialV1
            from (crate::OperationEffect)
            using operation_effect_material(value) {
            unit_variants {
                (
                    "OperationEffect::ReadOnly",
                    "OperationEffect::ReadOnly",
                    Model,
                    "OperationEffectMaterialV1{kind=read_only}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "read_only",
                    "OperationEffectMaterialV1::ReadOnly",
                    ReadOnly as "read_only",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "OperationEffect::ProviderIdempotent",
                    "OperationEffect::ProviderIdempotent",
                    Model,
                    "OperationEffectMaterialV1{kind=provider_idempotent}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "provider_idempotent",
                    "OperationEffectMaterialV1::ProviderIdempotent",
                    ProviderIdempotent as "provider_idempotent" {
                        (
                            "OperationEffect::ProviderIdempotent.side_effect_steps",
                            "OperationEffect::ProviderIdempotent.side_effect_steps",
                            Model,
                            "OperationEffectMaterialV1{kind=provider_idempotent}.value.side_effect_steps",
                            "normalized",
                            "step",
                            "nonempty_array",
                            "NonEmptyVec<ProviderIdempotentStepMaterialV1>",
                            "OperationEffectMaterialV1::ProviderIdempotent.side_effect_steps",
                            side_effect_steps:
                                Vec<ProviderIdempotentStepMaterialV1> = {
                                let mut steps = side_effect_steps
                                    .iter()
                                    .map(
                                        project_provider_idempotent_step_material
                                    )
                                    .collect::<Result<
                                        Vec<_>,
                                        CatalogError,
                                    >>()?;
                                steps.sort_by(|left, right| {
                                    left.step.cmp(&right.step)
                                });
                                steps
                            },
                        );
                    },
                );
            }
        }
        enum PaginationMaterialV1
            from (crate::PaginationPlan)
            using pagination_material(value) {
            unit_variants {
                (
                    "PaginationPlan::None",
                    "PaginationPlan::None",
                    Model,
                    "PaginationMaterialV1{kind=none}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "none",
                    "PaginationMaterialV1::None",
                    None as "none",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "PaginationPlan::Cursor",
                    "PaginationPlan::Cursor",
                    Model,
                    "PaginationMaterialV1{kind=cursor}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "cursor",
                    "PaginationMaterialV1::Cursor",
                    Cursor as "cursor" {
                        (
                            "PaginationPlan::Cursor.request_binding",
                            "PaginationPlan::Cursor.request_binding",
                            Model,
                            "PaginationMaterialV1{kind=cursor}.value.request_binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "PaginationMaterialV1::Cursor.request_binding",
                            request_binding: String = {
                                request_binding.clone()
                            },
                        );
                        (
                            "PaginationPlan::Cursor.response_pointer",
                            "PaginationPlan::Cursor.response_pointer",
                            Model,
                            "PaginationMaterialV1{kind=cursor}.value.response_pointer",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticJsonPointer",
                            "PaginationMaterialV1::Cursor.response_pointer",
                            response_pointer: String = {
                                response_pointer.clone()
                            },
                        );
                        (
                            "PaginationPlan::Cursor.bounds",
                            "PaginationPlan::Cursor.bounds",
                            Model,
                            "PaginationMaterialV1{kind=cursor}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "PaginationBoundsMaterialV1",
                            "PaginationMaterialV1::Cursor.bounds",
                            bounds: PaginationBoundsMaterialV1 = {
                                project_pagination_bounds_material(bounds)?
                            },
                        );
                    },
                );
                (
                    "PaginationPlan::OffsetLimit",
                    "PaginationPlan::OffsetLimit",
                    Model,
                    "PaginationMaterialV1{kind=offset_limit}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "offset_limit",
                    "PaginationMaterialV1::OffsetLimit",
                    OffsetLimit as "offset_limit" {
                        (
                            "PaginationPlan::OffsetLimit.offset_binding",
                            "PaginationPlan::OffsetLimit.offset_binding",
                            Model,
                            "PaginationMaterialV1{kind=offset_limit}.value.offset_binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "PaginationMaterialV1::OffsetLimit.offset_binding",
                            offset_binding: String = {
                                offset_binding.clone()
                            },
                        );
                        (
                            "PaginationPlan::OffsetLimit.limit_binding",
                            "PaginationPlan::OffsetLimit.limit_binding",
                            Model,
                            "PaginationMaterialV1{kind=offset_limit}.value.limit_binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "PaginationMaterialV1::OffsetLimit.limit_binding",
                            limit_binding: String = {
                                limit_binding.clone()
                            },
                        );
                        (
                            "PaginationPlan::OffsetLimit.initial_offset",
                            "PaginationPlan::OffsetLimit.initial_offset",
                            Model,
                            "PaginationMaterialV1{kind=offset_limit}.value.initial_offset",
                            "normalized",
                            "scalar",
                            "required",
                            "u64-string",
                            "PaginationMaterialV1::OffsetLimit.initial_offset",
                            initial_offset: String = {
                                initial_offset.to_string()
                            },
                        );
                        (
                            "PaginationPlan::OffsetLimit.page_size",
                            "PaginationPlan::OffsetLimit.page_size",
                            Model,
                            "PaginationMaterialV1{kind=offset_limit}.value.page_size",
                            "normalized",
                            "scalar",
                            "required",
                            "u32",
                            "PaginationMaterialV1::OffsetLimit.page_size",
                            page_size: u32 = {
                                page_size.get()
                            },
                        );
                        (
                            "PaginationPlan::OffsetLimit.bounds",
                            "PaginationPlan::OffsetLimit.bounds",
                            Model,
                            "PaginationMaterialV1{kind=offset_limit}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "PaginationBoundsMaterialV1",
                            "PaginationMaterialV1::OffsetLimit.bounds",
                            bounds: PaginationBoundsMaterialV1 = {
                                project_pagination_bounds_material(bounds)?
                            },
                        );
                    },
                );
                (
                    "PaginationPlan::PageNumber",
                    "PaginationPlan::PageNumber",
                    Model,
                    "PaginationMaterialV1{kind=page_number}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "page_number",
                    "PaginationMaterialV1::PageNumber",
                    PageNumber as "page_number" {
                        (
                            "PaginationPlan::PageNumber.page_binding",
                            "PaginationPlan::PageNumber.page_binding",
                            Model,
                            "PaginationMaterialV1{kind=page_number}.value.page_binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "PaginationMaterialV1::PageNumber.page_binding",
                            page_binding: String = {
                                page_binding.clone()
                            },
                        );
                        (
                            "PaginationPlan::PageNumber.page_size_binding",
                            "PaginationPlan::PageNumber.page_size_binding",
                            Model,
                            "PaginationMaterialV1{kind=page_number}.value.page_size_binding",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "PaginationMaterialV1::PageNumber.page_size_binding",
                            page_size_binding: String = {
                                page_size_binding.clone()
                            },
                        );
                        (
                            "PaginationPlan::PageNumber.initial_page",
                            "PaginationPlan::PageNumber.initial_page",
                            Model,
                            "PaginationMaterialV1{kind=page_number}.value.initial_page",
                            "normalized",
                            "scalar",
                            "required",
                            "u64-string",
                            "PaginationMaterialV1::PageNumber.initial_page",
                            initial_page: String = {
                                initial_page.get().to_string()
                            },
                        );
                        (
                            "PaginationPlan::PageNumber.page_size",
                            "PaginationPlan::PageNumber.page_size",
                            Model,
                            "PaginationMaterialV1{kind=page_number}.value.page_size",
                            "normalized",
                            "scalar",
                            "required",
                            "u32",
                            "PaginationMaterialV1::PageNumber.page_size",
                            page_size: u32 = {
                                page_size.get()
                            },
                        );
                        (
                            "PaginationPlan::PageNumber.bounds",
                            "PaginationPlan::PageNumber.bounds",
                            Model,
                            "PaginationMaterialV1{kind=page_number}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "PaginationBoundsMaterialV1",
                            "PaginationMaterialV1::PageNumber.bounds",
                            bounds: PaginationBoundsMaterialV1 = {
                                project_pagination_bounds_material(bounds)?
                            },
                        );
                    },
                );
                (
                    "PaginationPlan::LinkRelation",
                    "PaginationPlan::LinkRelation",
                    Model,
                    "PaginationMaterialV1{kind=link_relation}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "link_relation",
                    "PaginationMaterialV1::LinkRelation",
                    LinkRelation as "link_relation" {
                        (
                            "PaginationPlan::LinkRelation.relation",
                            "PaginationPlan::LinkRelation.relation",
                            Model,
                            "PaginationMaterialV1{kind=link_relation}.value.relation",
                            "normalized",
                            "scalar",
                            "required",
                            "string",
                            "PaginationMaterialV1::LinkRelation.relation",
                            relation: String = {
                                relation.clone()
                            },
                        );
                        (
                            "PaginationPlan::LinkRelation.selected_header",
                            "PaginationPlan::LinkRelation.selected_header",
                            Model,
                            "PaginationMaterialV1{kind=link_relation}.value.selected_header",
                            "normalized",
                            "scalar",
                            "required",
                            "SelectedResponseHeaderMaterialV1",
                            "PaginationMaterialV1::LinkRelation.selected_header",
                            selected_header:
                                SelectedResponseHeaderMaterialV1 = {
                                project_selected_header_material(
                                    selected_header,
                                )?
                            },
                        );
                        (
                            "PaginationPlan::LinkRelation.bounds",
                            "PaginationPlan::LinkRelation.bounds",
                            Model,
                            "PaginationMaterialV1{kind=link_relation}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "PaginationBoundsMaterialV1",
                            "PaginationMaterialV1::LinkRelation.bounds",
                            bounds: PaginationBoundsMaterialV1 = {
                                project_pagination_bounds_material(bounds)?
                            },
                        );
                    },
                );
                (
                    "PaginationPlan::Processor",
                    "PaginationPlan::Processor",
                    Model,
                    "PaginationMaterialV1{kind=processor}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "processor",
                    "PaginationMaterialV1::Processor",
                    Processor as "processor" {
                        (
                            "PaginationPlan::Processor.processor",
                            "PaginationPlan::Processor.processor",
                            Model,
                            "PaginationMaterialV1{kind=processor}.value.processor",
                            "normalized",
                            "scalar",
                            "required",
                            "VersionedProcessorMaterialV1",
                            "PaginationMaterialV1::Processor.processor",
                            processor: VersionedProcessorMaterialV1 = {
                                project_versioned_processor_material(
                                    processor,
                                )?
                            },
                        );
                        (
                            "PaginationPlan::Processor.bounds",
                            "PaginationPlan::Processor.bounds",
                            Model,
                            "PaginationMaterialV1{kind=processor}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "PaginationBoundsMaterialV1",
                            "PaginationMaterialV1::Processor.bounds",
                            bounds: PaginationBoundsMaterialV1 = {
                                project_pagination_bounds_material(bounds)?
                            },
                        );
                    },
                );
            }
        }
        enum ConnectorErrorClassMaterialV1
            from (ConnectorErrorClass)
            using connector_error_class_material(value) {
            unit_variants {
                (
                    "ConnectorErrorClass::Transport",
                    "ConnectorErrorClass::Transport",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=transport}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "transport",
                    "ConnectorErrorClassMaterialV1::Transport",
                    Transport as "transport",
                );
                (
                    "ConnectorErrorClass::Timeout",
                    "ConnectorErrorClass::Timeout",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=timeout}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "timeout",
                    "ConnectorErrorClassMaterialV1::Timeout",
                    Timeout as "timeout",
                );
                (
                    "ConnectorErrorClass::Http429",
                    "ConnectorErrorClass::Http429",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=http_429}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "http_429",
                    "ConnectorErrorClassMaterialV1::Http429",
                    Http429 as "http_429",
                );
                (
                    "ConnectorErrorClass::Http5xx",
                    "ConnectorErrorClass::Http5xx",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=http_5xx}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "http_5xx",
                    "ConnectorErrorClassMaterialV1::Http5xx",
                    Http5xx as "http_5xx",
                );
                (
                    "ConnectorErrorClass::Authentication",
                    "ConnectorErrorClass::Authentication",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=authentication}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "authentication",
                    "ConnectorErrorClassMaterialV1::Authentication",
                    Authentication as "authentication",
                );
                (
                    "ConnectorErrorClass::Validation",
                    "ConnectorErrorClass::Validation",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=validation}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "validation",
                    "ConnectorErrorClassMaterialV1::Validation",
                    Validation as "validation",
                );
                (
                    "ConnectorErrorClass::Permanent",
                    "ConnectorErrorClass::Permanent",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=permanent}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "permanent",
                    "ConnectorErrorClassMaterialV1::Permanent",
                    Permanent as "permanent",
                );
                (
                    "ConnectorErrorClass::Invariant",
                    "ConnectorErrorClass::Invariant",
                    ConnectorAbi,
                    "ConnectorErrorClassMaterialV1{kind=invariant}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "invariant",
                    "ConnectorErrorClassMaterialV1::Invariant",
                    Invariant as "invariant",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {}
        }
        enum RetryAfterMaterialV1
            from (crate::RetryAfterPolicy)
            using retry_after_material(value) {
            unit_variants {
                (
                    "RetryAfterPolicy::Never",
                    "RetryAfterPolicy::Never",
                    Model,
                    "RetryAfterMaterialV1{kind=never}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "never",
                    "RetryAfterMaterialV1::Never",
                    Never as "never",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "RetryAfterPolicy::RetryAfterHeader",
                    "RetryAfterPolicy::RetryAfterHeader",
                    Model,
                    "RetryAfterMaterialV1{kind=retry_after_header}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "retry_after_header",
                    "RetryAfterMaterialV1::RetryAfterHeader",
                    RetryAfterHeader as "retry_after_header" {
                        (
                            "RetryAfterPolicy::RetryAfterHeader.step",
                            "RetryAfterPolicy::RetryAfterHeader.step",
                            Model,
                            "RetryAfterMaterialV1{kind=retry_after_header}.value.step",
                            "normalized",
                            "scalar",
                            "required",
                            "CompiledStepId",
                            "RetryAfterMaterialV1::RetryAfterHeader.step",
                            step: String = {
                                step.as_str().to_owned()
                            },
                        );
                        (
                            "RetryAfterPolicy::RetryAfterHeader.capability",
                            "RetryAfterPolicy::RetryAfterHeader.capability",
                            Model,
                            "RetryAfterMaterialV1{kind=retry_after_header}.value.capability",
                            "normalized",
                            "scalar",
                            "required",
                            "CapabilityId",
                            "RetryAfterMaterialV1::RetryAfterHeader.capability",
                            capability: String = {
                                capability.as_str().to_owned()
                            },
                        );
                        (
                            "RetryAfterPolicy::RetryAfterHeader.maximum_seconds",
                            "RetryAfterPolicy::RetryAfterHeader.maximum_seconds",
                            Model,
                            "RetryAfterMaterialV1{kind=retry_after_header}.value.maximum_seconds",
                            "normalized",
                            "scalar",
                            "required",
                            "u32",
                            "RetryAfterMaterialV1::RetryAfterHeader.maximum_seconds",
                            maximum_seconds: u32 = {
                                maximum_seconds.get()
                            },
                        );
                    },
                );
            }
        }
        enum ErrorMatcherMaterialV1
            from (crate::ErrorMatcher)
            using error_matcher_material(value) {
            unit_variants {
                (
                    "ErrorMatcher::MalformedDeclaredSuccess",
                    "ErrorMatcher::MalformedDeclaredSuccess",
                    Model,
                    "ErrorMatcherMaterialV1{kind=malformed_declared_success}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "malformed_declared_success",
                    "ErrorMatcherMaterialV1::MalformedDeclaredSuccess",
                    MalformedDeclaredSuccess as
                        "malformed_declared_success",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {
                (
                    "ErrorMatcher::Status",
                    "ErrorMatcher::Status",
                    Model,
                    "ErrorMatcherMaterialV1{kind=status}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "status",
                    "ErrorMatcherMaterialV1::Status",
                    Status as "status"(
                        status: StatusRangeMaterialV1 = {
                            project_status_range_material(status)?
                        }
                    ) {
                        (
                            "ErrorMatcher::Status.minimum",
                            "StatusRange.minimum",
                            Model,
                            "ErrorMatcherMaterialV1{kind=status}.value.minimum",
                            "normalized",
                            "scalar",
                            "required",
                            "u16",
                            "StatusRangeMaterialV1.minimum",
                        );
                        (
                            "ErrorMatcher::Status.maximum",
                            "StatusRange.maximum",
                            Model,
                            "ErrorMatcherMaterialV1{kind=status}.value.maximum",
                            "normalized",
                            "scalar",
                            "required",
                            "u16",
                            "StatusRangeMaterialV1.maximum",
                        );
                    },
                );
            }
            struct_variants {
                (
                    "ErrorMatcher::ProviderCode",
                    "ErrorMatcher::ProviderCode",
                    Model,
                    "ErrorMatcherMaterialV1{kind=provider_code}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "provider_code",
                    "ErrorMatcherMaterialV1::ProviderCode",
                    ProviderCode as "provider_code" {
                        (
                            "ErrorMatcher::ProviderCode.pointer",
                            "ErrorMatcher::ProviderCode.pointer",
                            Model,
                            "ErrorMatcherMaterialV1{kind=provider_code}.value.pointer",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticJsonPointer",
                            "ErrorMatcherMaterialV1::ProviderCode.pointer",
                            pointer: String = {
                                pointer.clone()
                            },
                        );
                        (
                            "ErrorMatcher::ProviderCode.codes",
                            "ErrorMatcher::ProviderCode.codes",
                            Model,
                            "ErrorMatcherMaterialV1{kind=provider_code}.value.codes",
                            "normalized",
                            "lexical",
                            "nonempty_array",
                            "NonEmptyVec<StaticProviderCode>",
                            "ErrorMatcherMaterialV1::ProviderCode.codes",
                            codes: Vec<String> = {
                                let mut codes = codes.to_vec();
                                codes.sort();
                                codes
                            },
                        );
                    },
                );
                (
                    "ErrorMatcher::Header",
                    "ErrorMatcher::Header",
                    Model,
                    "ErrorMatcherMaterialV1{kind=header}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "header",
                    "ErrorMatcherMaterialV1::Header",
                    Header as "header" {
                        (
                            "ErrorMatcher::Header.name",
                            "ErrorMatcher::Header.name",
                            Model,
                            "ErrorMatcherMaterialV1{kind=header}.value.name",
                            "normalized",
                            "scalar",
                            "required",
                            "StaticHeaderName",
                            "ErrorMatcherMaterialV1::Header.name",
                            name: String = {
                                name.clone()
                            },
                        );
                        (
                            "ErrorMatcher::Header.values",
                            "ErrorMatcher::Header.values",
                            Model,
                            "ErrorMatcherMaterialV1{kind=header}.value.values",
                            "normalized",
                            "lexical",
                            "nonempty_array",
                            "NonEmptyVec<StaticHeaderValue>",
                            "ErrorMatcherMaterialV1::Header.values",
                            values: Vec<String> = {
                                let mut values = values.clone();
                                values.sort();
                                values
                            },
                        );
                    },
                );
            }
        }
        #[allow(clippy::large_enum_variant)]
        enum SemanticTriggerMaterialV1
            from (crate::TriggerSpec)
            using semantic_trigger_material(
                value;
                value_language_epoch: u32 = {
                    validate_semantic_value_language_epoch(
                        value_language_epoch,
                    )?
                }
            ) {
            unit_variants {}
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "TriggerSpec::Webhook",
                    "TriggerSpec::Webhook",
                    Model,
                    "SemanticTriggerMaterialV1{kind=webhook}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "webhook",
                    "SemanticTriggerMaterialV1::Webhook",
                    Webhook as "webhook" {
                        (
                            "TriggerSpec::Webhook.connector",
                            "TriggerSpec::Webhook.connector",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.connector",
                            "normalized",
                            "scalar",
                            "required",
                            "ConnectorId",
                            "SemanticTriggerMaterialV1::Webhook.connector",
                            connector: String = {
                                connector.as_str().to_owned()
                            },
                        );
                        (
                            "TriggerSpec::Webhook.connector_version",
                            "TriggerSpec::Webhook.connector_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.connector_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Webhook.connector_version",
                            connector_version: StableSemver = {
                                *connector_version
                            },
                        );
                        (
                            "TriggerSpec::Webhook.trigger",
                            "TriggerSpec::Webhook.trigger",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.trigger",
                            "normalized",
                            "scalar",
                            "required",
                            "TriggerId",
                            "SemanticTriggerMaterialV1::Webhook.trigger",
                            trigger: String = {
                                trigger.as_str().to_owned()
                            },
                        );
                        (
                            "TriggerSpec::Webhook.trigger_version",
                            "TriggerSpec::Webhook.trigger_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.trigger_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Webhook.trigger_version",
                            trigger_version: StableSemver = {
                                *trigger_version
                            },
                        );
                        (
                            "TriggerSpec::Webhook.event_version",
                            "TriggerSpec::Webhook.event_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.event_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Webhook.event_version",
                            event_version: StableSemver = {
                                *event_version
                            },
                        );
                        (
                            "TriggerSpec::Webhook.runtime_abi_epoch",
                            "TriggerSpec::Webhook.runtime_abi_epoch",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.runtime_abi_epoch",
                            "normalized",
                            "scalar",
                            "required",
                            "Epoch",
                            "SemanticTriggerMaterialV1::Webhook.runtime_abi_epoch",
                            runtime_abi_epoch: u32 = {
                                *runtime_abi_epoch
                            },
                        );
                        (
                            "TriggerSpec::Webhook.authenticator",
                            "TriggerSpec::Webhook.authenticator",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.authenticator",
                            "normalized",
                            "scalar",
                            "required",
                            "VersionedProcessorMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.authenticator",
                            authenticator:
                                VersionedProcessorMaterialV1 = {
                                project_versioned_processor_material(
                                    authenticator,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.codec",
                            "TriggerSpec::Webhook.codec",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.codec",
                            "normalized",
                            "scalar",
                            "required",
                            "VersionedProcessorMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.codec",
                            codec: VersionedProcessorMaterialV1 = {
                                project_versioned_processor_material(codec)?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.normalizer",
                            "TriggerSpec::Webhook.normalizer",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.normalizer",
                            "normalized",
                            "scalar",
                            "required",
                            "VersionedProcessorMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.normalizer",
                            normalizer: VersionedProcessorMaterialV1 = {
                                project_versioned_processor_material(
                                    normalizer,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.selected_headers",
                            "TriggerSpec::Webhook.selected_headers",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.selected_headers",
                            "normalized",
                            "lexical",
                            "empty_array",
                            "Vec<StaticHeaderName>",
                            "SemanticTriggerMaterialV1::Webhook.selected_headers",
                            selected_headers: Vec<String> = {
                                let mut headers = selected_headers.clone();
                                headers.sort();
                                headers
                            },
                        );
                        (
                            "TriggerSpec::Webhook.raw_body_max_bytes",
                            "TriggerSpec::Webhook.raw_body_max_bytes",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.raw_body_max_bytes",
                            "normalized",
                            "scalar",
                            "required",
                            "u32",
                            "SemanticTriggerMaterialV1::Webhook.raw_body_max_bytes",
                            raw_body_max_bytes: u32 = {
                                raw_body_max_bytes.get()
                            },
                        );
                        (
                            "TriggerSpec::Webhook.timestamp_window_ms",
                            "TriggerSpec::Webhook.timestamp_window_ms",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.timestamp_window_ms",
                            "normalized",
                            "scalar",
                            "required",
                            "u64-string",
                            "SemanticTriggerMaterialV1::Webhook.timestamp_window_ms",
                            timestamp_window_ms: String = {
                                timestamp_window_ms.get().to_string()
                            },
                        );
                        (
                            "TriggerSpec::Webhook.event_id",
                            "TriggerSpec::Webhook.event_id",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.event_id",
                            "normalized",
                            "scalar",
                            "required",
                            "ValueContractMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.event_id",
                            #[serde(
                                deserialize_with =
                                    "deserialize_value_contract_material"
                            )]
                            event_id: ValueContractMaterialV1 = {
                                value_contract_material(
                                    event_id,
                                    value_language_epoch,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.event_type",
                            "TriggerSpec::Webhook.event_type",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.event_type",
                            "normalized",
                            "scalar",
                            "required",
                            "ValueContractMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.event_type",
                            #[serde(
                                deserialize_with =
                                    "deserialize_value_contract_material"
                            )]
                            event_type: ValueContractMaterialV1 = {
                                value_contract_material(
                                    event_type,
                                    value_language_epoch,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.output",
                            "TriggerSpec::Webhook.output",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.output",
                            "normalized",
                            "scalar",
                            "required",
                            "ValueContractMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.output",
                            #[serde(
                                deserialize_with =
                                    "deserialize_value_contract_material"
                            )]
                            output: ValueContractMaterialV1 = {
                                value_contract_material(
                                    output,
                                    value_language_epoch,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.redaction",
                            "TriggerSpec::Webhook.redaction",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.redaction",
                            "normalized",
                            "scalar",
                            "required",
                            "RedactionMaterialV1",
                            "SemanticTriggerMaterialV1::Webhook.redaction",
                            redaction: RedactionMaterialV1 = {
                                redaction_material(redaction)?
                            },
                        );
                        (
                            "TriggerSpec::Webhook.subscription_operations",
                            "TriggerSpec::Webhook.subscription_operations",
                            Model,
                            "SemanticTriggerMaterialV1{kind=webhook}.value.subscription_operations",
                            "normalized",
                            "scalar",
                            "explicit_null",
                            "Option<SubscriptionOperationIdsMaterialV1>",
                            "SemanticTriggerMaterialV1::Webhook.subscription_operations",
                            subscription_operations:
                                Option<SubscriptionOperationIdsMaterialV1> = {
                                subscription_operations
                                    .as_ref()
                                    .map(
                                        project_subscription_operation_ids_material
                                    )
                                    .transpose()?
                            },
                        );
                    },
                );
                (
                    "TriggerSpec::Poll",
                    "TriggerSpec::Poll",
                    Model,
                    "SemanticTriggerMaterialV1{kind=poll}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "poll",
                    "SemanticTriggerMaterialV1::Poll",
                    Poll as "poll" {
                        (
                            "TriggerSpec::Poll.connector",
                            "TriggerSpec::Poll.connector",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.connector",
                            "normalized",
                            "scalar",
                            "required",
                            "ConnectorId",
                            "SemanticTriggerMaterialV1::Poll.connector",
                            connector: String = {
                                connector.as_str().to_owned()
                            },
                        );
                        (
                            "TriggerSpec::Poll.connector_version",
                            "TriggerSpec::Poll.connector_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.connector_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Poll.connector_version",
                            connector_version: StableSemver = {
                                *connector_version
                            },
                        );
                        (
                            "TriggerSpec::Poll.trigger",
                            "TriggerSpec::Poll.trigger",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.trigger",
                            "normalized",
                            "scalar",
                            "required",
                            "TriggerId",
                            "SemanticTriggerMaterialV1::Poll.trigger",
                            trigger: String = {
                                trigger.as_str().to_owned()
                            },
                        );
                        (
                            "TriggerSpec::Poll.trigger_version",
                            "TriggerSpec::Poll.trigger_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.trigger_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Poll.trigger_version",
                            trigger_version: StableSemver = {
                                *trigger_version
                            },
                        );
                        (
                            "TriggerSpec::Poll.event_version",
                            "TriggerSpec::Poll.event_version",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.event_version",
                            "normalized",
                            "scalar",
                            "required",
                            "StableSemver",
                            "SemanticTriggerMaterialV1::Poll.event_version",
                            event_version: StableSemver = {
                                *event_version
                            },
                        );
                        (
                            "TriggerSpec::Poll.runtime_abi_epoch",
                            "TriggerSpec::Poll.runtime_abi_epoch",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.runtime_abi_epoch",
                            "normalized",
                            "scalar",
                            "required",
                            "Epoch",
                            "SemanticTriggerMaterialV1::Poll.runtime_abi_epoch",
                            runtime_abi_epoch: u32 = {
                                *runtime_abi_epoch
                            },
                        );
                        (
                            "TriggerSpec::Poll.checkpoint",
                            "TriggerSpec::Poll.checkpoint",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.checkpoint",
                            "normalized",
                            "scalar",
                            "required",
                            "ValueContractMaterialV1",
                            "SemanticTriggerMaterialV1::Poll.checkpoint",
                            #[serde(
                                deserialize_with =
                                    "deserialize_value_contract_material"
                            )]
                            checkpoint: ValueContractMaterialV1 = {
                                value_contract_material(
                                    checkpoint,
                                    value_language_epoch,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Poll.processor",
                            "TriggerSpec::Poll.processor",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.processor",
                            "normalized",
                            "scalar",
                            "required",
                            "VersionedProcessorMaterialV1",
                            "SemanticTriggerMaterialV1::Poll.processor",
                            processor: VersionedProcessorMaterialV1 = {
                                project_versioned_processor_material(
                                    processor,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Poll.event_type",
                            "TriggerSpec::Poll.event_type",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.event_type",
                            "normalized",
                            "scalar",
                            "required",
                            "ValueContractMaterialV1",
                            "SemanticTriggerMaterialV1::Poll.event_type",
                            #[serde(
                                deserialize_with =
                                    "deserialize_value_contract_material"
                            )]
                            event_type: ValueContractMaterialV1 = {
                                value_contract_material(
                                    event_type,
                                    value_language_epoch,
                                )?
                            },
                        );
                        (
                            "TriggerSpec::Poll.per_poll_event_limit",
                            "TriggerSpec::Poll.per_poll_event_limit",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.per_poll_event_limit",
                            "normalized",
                            "scalar",
                            "required",
                            "u32",
                            "SemanticTriggerMaterialV1::Poll.per_poll_event_limit",
                            per_poll_event_limit: u32 = {
                                per_poll_event_limit.get()
                            },
                        );
                        (
                            "TriggerSpec::Poll.bounds",
                            "TriggerSpec::Poll.bounds",
                            Model,
                            "SemanticTriggerMaterialV1{kind=poll}.value.bounds",
                            "normalized",
                            "scalar",
                            "required",
                            "OperationBoundsMaterialV1",
                            "SemanticTriggerMaterialV1::Poll.bounds",
                            bounds: OperationBoundsMaterialV1 = {
                                project_operation_bounds_material(bounds)?
                            },
                        );
                    },
                );
            }
        }
        enum NetworkPolicyMaterialV1
            from (crate::NetworkPolicy)
            using network_policy_material(value) {
            unit_variants {
                (
                    "NetworkPolicy::PublicOnly",
                    "NetworkPolicy::PublicOnly",
                    Model,
                    "NetworkPolicyMaterialV1{kind=public_only}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "public_only",
                    "NetworkPolicyMaterialV1::PublicOnly",
                    PublicOnly as "public_only",
                );
            }
            composite_unit_variants {}
            tuple_variants {}
            tuple_struct_variants {}
            struct_variants {
                (
                    "NetworkPolicy::PrivateAllowed",
                    "NetworkPolicy::PrivateAllowed",
                    Model,
                    "NetworkPolicyMaterialV1{kind=private_allowed}.kind",
                    "normalized",
                    "scalar",
                    "required",
                    "private_allowed",
                    "NetworkPolicyMaterialV1::PrivateAllowed",
                    PrivateAllowed as "private_allowed" {
                        (
                            "NetworkPolicy::PrivateAllowed.policy",
                            "NetworkPolicy::PrivateAllowed.policy",
                            Model,
                            "NetworkPolicyMaterialV1{kind=private_allowed}.value.policy",
                            "normalized",
                            "scalar",
                            "required",
                            "Id",
                            "NetworkPolicyMaterialV1::PrivateAllowed.policy",
                            policy: String = {
                                policy.clone()
                            },
                        );
                    },
                );
            }
        }
    }
    singleton_enums {
        enum HttpsMaterialV1
            from (crate::HttpsOnly)
            using https_material(value) {
            HttpsOnly as "https"
        }
    }
}
provenance_projection {
    owner_paths {
        (
            "StableSemver.major",
            "StableSemver.major",
            Model,
            "provenance",
            "StableSemver.major",
            "normalized",
            "scalar",
            "required",
            "u32",
            "StableSemver.major",
            Model,
            Provenance,
            Mutable,
        );

        (
            "StableSemver.minor",
            "StableSemver.minor",
            Model,
            "provenance",
            "StableSemver.minor",
            "normalized",
            "scalar",
            "required",
            "u32",
            "StableSemver.minor",
            Model,
            Provenance,
            Mutable,
        );

        (
            "StableSemver.patch",
            "StableSemver.patch",
            Model,
            "provenance",
            "StableSemver.patch",
            "normalized",
            "scalar",
            "required",
            "u32",
            "StableSemver.patch",
            Model,
            Provenance,
            Mutable,
        );

        (
            "ConnectorManifest.connector",
            "ConnectorManifest.connector",
            Model,
            "provenance",
            "ProvenanceMaterialV1.connector.id",
            "normalized",
            "scalar",
            "required",
            "ConnectorId",
            "ProvenanceConnectorIdentity.id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ConnectorManifest.connector_version",
            "ConnectorManifest.connector_version",
            Model,
            "provenance",
            "ProvenanceMaterialV1.connector.version",
            "normalized",
            "scalar",
            "required",
            "StableSemver",
            "ProvenanceConnectorIdentity.version",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ConnectorManifest.provenance",
            "ConnectorManifest.provenance",
            Model,
            "provenance",
            "ProvenanceMaterialV1.manifest_references",
            "normalized",
            "source_record_id",
            "nonempty_array",
            "NonEmptyVec<ManifestProvenanceMaterialV1>",
            "ProvenanceMaterialV1.manifest_references",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.source_record_id",
            "ManifestProvenanceReference.source_record_id",
            Model,
            "provenance",
            "ManifestProvenanceMaterialV1.source_record_id",
            "normalized",
            "scalar",
            "required",
            "SourceRecordId",
            "ManifestProvenanceMaterialV1.source_record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes",
            "ManifestProvenanceReference.artifact_hashes",
            Model,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes",
            "normalized",
            "artifact_id",
            "nonempty_array",
            "NonEmptyVec<ArtifactHashMaterialV1>",
            "ManifestProvenanceMaterialV1.artifact_hashes",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.license_id",
            "ManifestProvenanceReference.license_id",
            Model,
            "provenance",
            "ManifestProvenanceMaterialV1.license_id",
            "normalized",
            "scalar",
            "required",
            "LicenseIdentity",
            "ManifestProvenanceMaterialV1.license_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.notice_id",
            "ManifestProvenanceReference.notice_id",
            Model,
            "provenance",
            "ManifestProvenanceMaterialV1.notice_id",
            "normalized",
            "scalar",
            "required",
            "NoticeId",
            "ManifestProvenanceMaterialV1.notice_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.contract_facts",
            "ManifestProvenanceReference.contract_facts",
            Model,
            "provenance",
            "ManifestProvenanceMaterialV1.contract_fact_origins",
            "normalized",
            "use_site",
            "empty_array",
            "Vec<ResolvedFactOriginMaterialV1>",
            "ManifestProvenanceMaterialV1.contract_fact_origins",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ResolvedContractFactBinding.use_site",
            "ResolvedContractFactBinding.use_site",
            Model,
            "provenance",
            "ResolvedFactOriginMaterialV1.use_site",
            "normalized",
            "scalar",
            "required",
            "Id",
            "ResolvedFactOriginMaterialV1.use_site",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ResolvedContractFactBinding.fact",
            "ResolvedContractFactBinding.fact",
            Model,
            "provenance",
            "ResolvedFactOriginMaterialV1.origin",
            "normalized",
            "scalar",
            "required",
            "ResolvedFactOrigin",
            "ResolvedFactOriginMaterialV1.origin",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ContractFact::ProviderEvidence.source_record_id",
            "ContractFact::ProviderEvidence.source_record_id",
            Source,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.source_record_id",
            "normalized",
            "scalar",
            "required",
            "SourceRecordId",
            "ResolvedFactOriginV1::ProviderEvidence.source_record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ContractFact::ProviderEvidence.fact_id",
            "ContractFact::ProviderEvidence.fact_id",
            Source,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.fact_id",
            "normalized",
            "scalar",
            "required",
            "ProviderFactId",
            "ResolvedFactOriginV1::ProviderEvidence.fact_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ContractFact::DonatPolicy.policy_id",
            "ContractFact::DonatPolicy.policy_id",
            Source,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=donat_policy}.value.policy_id",
            "normalized",
            "scalar",
            "required",
            "DonatPolicyId",
            "ResolvedFactOriginV1::DonatPolicy.policy_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ContractFact::ProviderEvidence",
            "ContractFact::ProviderEvidence",
            Source,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=provider_evidence}.kind",
            "normalized",
            "scalar",
            "required",
            "provider_evidence",
            "ResolvedFactOriginV1::ProviderEvidence",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ContractFact::DonatPolicy",
            "ContractFact::DonatPolicy",
            Source,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=donat_policy}.kind",
            "normalized",
            "scalar",
            "required",
            "donat_policy",
            "ResolvedFactOriginV1::DonatPolicy",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].artifact_id",
            "ArtifactHash.artifact_id",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].artifact_id",
            "normalized",
            "scalar",
            "required",
            "ArtifactId",
            "ArtifactHashMaterialV1.artifact_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].algorithm",
            "ArtifactHash.algorithm",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm",
            "normalized",
            "scalar",
            "required",
            "HashAlgorithm",
            "ArtifactHashMaterialV1.algorithm",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].digest",
            "ArtifactHash.digest",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].digest",
            "normalized",
            "scalar",
            "required",
            "Hash256_or_Hash512",
            "ArtifactHashMaterialV1.digest",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].path",
            "ArtifactHash.path",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].path",
            "normalized",
            "scalar",
            "explicit_null",
            "Option<SourcePath>",
            "ArtifactHashMaterialV1.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].algorithm::Sha256",
            "HashAlgorithm::Sha256",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha256}.kind",
            "normalized",
            "scalar",
            "required",
            "sha256",
            "HashAlgorithmMaterialV1::Sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ManifestProvenanceReference.artifact_hashes[].algorithm::Sha512",
            "HashAlgorithm::Sha512",
            Source,
            "provenance",
            "ManifestProvenanceMaterialV1.artifact_hashes[].algorithm{kind=sha512}.kind",
            "normalized",
            "scalar",
            "required",
            "sha512",
            "HashAlgorithmMaterialV1::Sha512",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactProviderArtifact.provider",
            "ExactProviderArtifact.provider",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].provider",
            "normalized",
            "scalar",
            "required",
            "string",
            "ProviderEvidenceOriginMaterialV1.provider",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactProviderArtifact.evidence",
            "ExactProviderArtifact.evidence",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence",
            "normalized",
            "canonical_source_identity",
            "nonempty_array",
            "NonEmptyVec<ProviderEvidenceOriginEntryMaterialV1>",
            "ProviderEvidenceOriginMaterialV1.evidence",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderEvidenceArtifact.accessed_on",
            "ProviderEvidenceArtifact.accessed_on",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].accessed_on",
            "normalized",
            "scalar",
            "required",
            "Date",
            "ProviderEvidenceOriginEntryMaterialV1.accessed_on",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderEvidenceArtifact.content_sha256",
            "ProviderEvidenceArtifact.content_sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].content_sha256",
            "normalized",
            "scalar",
            "required",
            "Hash256",
            "ProviderEvidenceOriginEntryMaterialV1.content_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderEvidenceArtifact.source",
            "ProviderEvidenceArtifact.source",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source",
            "normalized",
            "scalar",
            "required",
            "ImmutableProviderEvidenceSource",
            "ProviderEvidenceOriginEntryMaterialV1.source",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderEvidenceArtifact.terms",
            "ProviderEvidenceArtifact.terms",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms",
            "normalized",
            "scalar",
            "required",
            "EvidenceTermsMaterialV1",
            "ProviderEvidenceOriginEntryMaterialV1.terms",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderEvidenceArtifact.facts",
            "ProviderEvidenceArtifact.facts",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts",
            "normalized",
            "fact_id",
            "nonempty_array",
            "ProviderEvidenceOriginFactMaterialV1",
            "ProviderEvidenceOriginEntryMaterialV1.facts",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::RepositoryFile",
            "ImmutableProviderEvidenceSource::RepositoryFile",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.kind",
            "normalized",
            "scalar",
            "required",
            "repository_file",
            "ProviderEvidenceSourceMaterialV1::RepositoryFile",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::VersionedArtifact",
            "ImmutableProviderEvidenceSource::VersionedArtifact",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.kind",
            "normalized",
            "scalar",
            "required",
            "versioned_artifact",
            "ProviderEvidenceSourceMaterialV1::VersionedArtifact",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::RepositoryFile.repository",
            "ImmutableProviderEvidenceSource::RepositoryFile.repository",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.repository",
            "normalized",
            "scalar",
            "required",
            "RepositoryUrl",
            "ProviderEvidenceSourceMaterialV1::RepositoryFile.repository",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::RepositoryFile.commit",
            "ImmutableProviderEvidenceSource::RepositoryFile.commit",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.commit",
            "normalized",
            "scalar",
            "required",
            "GitCommit",
            "ProviderEvidenceSourceMaterialV1::RepositoryFile.commit",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::RepositoryFile.path",
            "ImmutableProviderEvidenceSource::RepositoryFile.path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=repository_file}.value.path",
            "normalized",
            "scalar",
            "required",
            "SourcePath",
            "ProviderEvidenceSourceMaterialV1::RepositoryFile.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::VersionedArtifact.url",
            "ImmutableProviderEvidenceSource::VersionedArtifact.url",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.url",
            "normalized",
            "scalar",
            "required",
            "ExactHttpsUrl",
            "ProviderEvidenceSourceMaterialV1::VersionedArtifact.url",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
            "ImmutableProviderEvidenceSource::VersionedArtifact.provider_revision",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].source{kind=versioned_artifact}.value.provider_revision",
            "normalized",
            "scalar",
            "required",
            "NonEmptyString",
            "ProviderEvidenceSourceMaterialV1::VersionedArtifact.provider_revision",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderFact.fact_id",
            "ProviderFact.fact_id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].fact_id",
            "normalized",
            "scalar",
            "required",
            "ProviderFactId",
            "ProviderEvidenceOriginFactMaterialV1.fact_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ProviderFact.location",
            "ProviderFact.location",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location",
            "normalized",
            "scalar",
            "required",
            "ExactFactLocationMaterialV1",
            "ProviderEvidenceOriginFactMaterialV1.location",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::JsonPointer",
            "ExactFactLocation::JsonPointer",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.kind",
            "normalized",
            "scalar",
            "required",
            "json_pointer",
            "ExactFactLocationMaterialV1::JsonPointer",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::DocumentSection",
            "ExactFactLocation::DocumentSection",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.kind",
            "normalized",
            "scalar",
            "required",
            "document_section",
            "ExactFactLocationMaterialV1::DocumentSection",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::JsonPointer.path",
            "ExactFactLocation::JsonPointer.path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.path",
            "normalized",
            "scalar",
            "required",
            "SourcePath",
            "ExactFactLocationMaterialV1::JsonPointer.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::JsonPointer.pointer",
            "ExactFactLocation::JsonPointer.pointer",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=json_pointer}.value.pointer",
            "normalized",
            "scalar",
            "required",
            "StaticJsonPointer",
            "ExactFactLocationMaterialV1::JsonPointer.pointer",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::DocumentSection.path",
            "ExactFactLocation::DocumentSection.path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.path",
            "normalized",
            "scalar",
            "required",
            "SourcePath",
            "ExactFactLocationMaterialV1::DocumentSection.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "ExactFactLocation::DocumentSection.section",
            "ExactFactLocation::DocumentSection.section",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].facts[].location{kind=document_section}.value.section",
            "normalized",
            "scalar",
            "required",
            "string",
            "ExactFactLocationMaterialV1::DocumentSection.section",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::Permissive",
            "EvidenceTermsDisposition::Permissive",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.kind",
            "normalized",
            "scalar",
            "required",
            "permissive",
            "EvidenceTermsMaterialV1::Permissive",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::ReviewedUse",
            "EvidenceTermsDisposition::ReviewedUse",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.kind",
            "normalized",
            "scalar",
            "required",
            "reviewed_use",
            "EvidenceTermsMaterialV1::ReviewedUse",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::Rejected",
            "EvidenceTermsDisposition::Rejected",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.kind",
            "normalized",
            "scalar",
            "required",
            "rejected",
            "EvidenceTermsMaterialV1::Rejected",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::Permissive.license",
            "EvidenceTermsDisposition::Permissive.license",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.license",
            "normalized",
            "scalar",
            "required",
            "LicenseDecisionMaterialV1",
            "EvidenceTermsMaterialV1::Permissive.license",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::Permissive.evidence_url",
            "EvidenceTermsDisposition::Permissive.evidence_url",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=permissive}.value.evidence_url",
            "normalized",
            "scalar",
            "required",
            "ExactHttpsUrl",
            "EvidenceTermsMaterialV1::Permissive.evidence_url",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::ReviewedUse.decision_id",
            "EvidenceTermsDisposition::ReviewedUse.decision_id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.decision_id",
            "normalized",
            "scalar",
            "required",
            "ReviewDecisionId",
            "EvidenceTermsMaterialV1::ReviewedUse.decision_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::ReviewedUse.evidence_url",
            "EvidenceTermsDisposition::ReviewedUse.evidence_url",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=reviewed_use}.value.evidence_url",
            "normalized",
            "scalar",
            "required",
            "ExactHttpsUrl",
            "EvidenceTermsMaterialV1::ReviewedUse.evidence_url",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EvidenceTermsDisposition::Rejected.finding",
            "EvidenceTermsDisposition::Rejected.finding",
            Source,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence[].evidence[].terms{kind=rejected}.value.finding",
            "normalized",
            "scalar",
            "required",
            "FindingId",
            "EvidenceTermsMaterialV1::Rejected.finding",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DonatOwnedSource.files[].path",
            "RepoFileHash.path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.files[].path",
            "normalized",
            "source_record_id_then_path",
            "required",
            "RepoPath",
            "FileDecisionMaterialV1.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DonatOwnedSource.files[].sha256",
            "RepoFileHash.sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.files[].sha256",
            "normalized",
            "source_record_id_then_path",
            "required",
            "Hash256",
            "FileDecisionMaterialV1.sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Permissive",
            "LicenseDecision::Permissive",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=permissive}.kind",
            "normalized",
            "canonical_bytes",
            "required",
            "permissive",
            "LicenseDecisionMaterialV1::Permissive",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::WrittenGrant",
            "LicenseDecision::WrittenGrant",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=written_grant}.kind",
            "normalized",
            "canonical_bytes",
            "required",
            "written_grant",
            "LicenseDecisionMaterialV1::WrittenGrant",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Rejected",
            "LicenseDecision::Rejected",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=rejected}.kind",
            "normalized",
            "canonical_bytes",
            "required",
            "rejected",
            "LicenseDecisionMaterialV1::Rejected",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Permissive.spdx_id",
            "LicenseDecision::Permissive.spdx_id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.spdx_id",
            "normalized",
            "canonical_bytes",
            "required",
            "string",
            "LicenseDecisionMaterialV1::Permissive.spdx_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Permissive.selected_dual_license_branch",
            "LicenseDecision::Permissive.selected_dual_license_branch",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.selected_dual_license_branch",
            "normalized",
            "canonical_bytes",
            "explicit_null",
            "Option<string>",
            "LicenseDecisionMaterialV1::Permissive.selected_dual_license_branch",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Permissive.license_file_path",
            "LicenseDecision::Permissive.license_file_path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_path",
            "normalized",
            "canonical_bytes",
            "required",
            "SourcePath",
            "LicenseDecisionMaterialV1::Permissive.license_file_path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Permissive.license_file_sha256",
            "LicenseDecision::Permissive.license_file_sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=permissive}.value.license_file_sha256",
            "normalized",
            "canonical_bytes",
            "required",
            "Hash256",
            "LicenseDecisionMaterialV1::Permissive.license_file_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::WrittenGrant.decision_id",
            "LicenseDecision::WrittenGrant.decision_id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.decision_id",
            "normalized",
            "canonical_bytes",
            "required",
            "ReviewDecisionId",
            "LicenseDecisionMaterialV1::WrittenGrant.decision_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::WrittenGrant.grant_sha256",
            "LicenseDecision::WrittenGrant.grant_sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=written_grant}.value.grant_sha256",
            "normalized",
            "canonical_bytes",
            "required",
            "Hash256",
            "LicenseDecisionMaterialV1::WrittenGrant.grant_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "LicenseDecision::Rejected.finding",
            "LicenseDecision::Rejected.finding",
            Source,
            "provenance",
            "ProvenanceMaterialV1.licenses[]{kind=rejected}.value.finding",
            "normalized",
            "canonical_bytes",
            "required",
            "FindingId",
            "LicenseDecisionMaterialV1::Rejected.finding",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "NoticeIdentity.id",
            "NoticeIdentity.id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.notices[].id",
            "normalized",
            "id",
            "required",
            "NoticeId",
            "NoticeMaterialV1.id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "NoticeIdentity.license_file_path",
            "NoticeIdentity.license_file_path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.notices[].license_file_path",
            "normalized",
            "id",
            "required",
            "SourcePath",
            "NoticeMaterialV1.license_file_path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "NoticeIdentity.license_file_sha256",
            "NoticeIdentity.license_file_sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.notices[].license_file_sha256",
            "normalized",
            "id",
            "required",
            "Hash256",
            "NoticeMaterialV1.license_file_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "NoticeIdentity.required_copyright_lines",
            "NoticeIdentity.required_copyright_lines",
            Source,
            "provenance",
            "ProvenanceMaterialV1.notices[].required_copyright_lines",
            "normalized",
            "declared",
            "empty_array",
            "Vec<string>",
            "NoticeMaterialV1.required_copyright_lines",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "NoticeIdentity.notice_bundle_destination",
            "NoticeIdentity.notice_bundle_destination",
            Source,
            "provenance",
            "ProvenanceMaterialV1.notices[].notice_bundle_destination",
            "normalized",
            "id",
            "required",
            "RepoPath",
            "NoticeMaterialV1.notice_bundle_destination",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDecision.dependency",
            "DependencyDecision.dependency",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].dependency",
            "normalized",
            "dependency",
            "required",
            "Id",
            "DependencyDecisionMaterialV1.dependency",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDecision.disposition",
            "DependencyDecision.disposition",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition",
            "normalized",
            "dependency",
            "required",
            "DependencyDispositionMaterialV1",
            "DependencyDecisionMaterialV1.disposition",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::Shipped",
            "DependencyDisposition::Shipped",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.kind",
            "normalized",
            "dependency",
            "required",
            "shipped",
            "DependencyDispositionMaterialV1::Shipped",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::BuildOnly",
            "DependencyDisposition::BuildOnly",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.kind",
            "normalized",
            "dependency",
            "required",
            "build_only",
            "DependencyDispositionMaterialV1::BuildOnly",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::TypeOnlyReplaced",
            "DependencyDisposition::TypeOnlyReplaced",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.kind",
            "normalized",
            "dependency",
            "required",
            "type_only_replaced",
            "DependencyDispositionMaterialV1::TypeOnlyReplaced",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::BehaviorOnly",
            "DependencyDisposition::BehaviorOnly",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.kind",
            "normalized",
            "dependency",
            "required",
            "behavior_only",
            "DependencyDispositionMaterialV1::BehaviorOnly",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::Rejected",
            "DependencyDisposition::Rejected",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.kind",
            "normalized",
            "dependency",
            "required",
            "rejected",
            "DependencyDispositionMaterialV1::Rejected",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::Shipped.license",
            "DependencyDisposition::Shipped.license",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=shipped}.value.license",
            "normalized",
            "dependency",
            "required",
            "LicenseDecisionMaterialV1",
            "DependencyDispositionMaterialV1::Shipped.license",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::BuildOnly.license",
            "DependencyDisposition::BuildOnly.license",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=build_only}.value.license",
            "normalized",
            "dependency",
            "required",
            "LicenseDecisionMaterialV1",
            "DependencyDispositionMaterialV1::BuildOnly.license",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::TypeOnlyReplaced.replacement",
            "DependencyDisposition::TypeOnlyReplaced.replacement",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=type_only_replaced}.value.replacement",
            "normalized",
            "dependency",
            "required",
            "Id",
            "DependencyDispositionMaterialV1::TypeOnlyReplaced.replacement",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::BehaviorOnly.reason",
            "DependencyDisposition::BehaviorOnly.reason",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=behavior_only}.value.reason",
            "normalized",
            "dependency",
            "required",
            "FindingId",
            "DependencyDispositionMaterialV1::BehaviorOnly.reason",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "DependencyDisposition::Rejected.finding",
            "DependencyDisposition::Rejected.finding",
            Source,
            "provenance",
            "ProvenanceMaterialV1.dependencies[].disposition{kind=rejected}.value.finding",
            "normalized",
            "dependency",
            "required",
            "FindingId",
            "DependencyDispositionMaterialV1::Rejected.finding",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDecision.material_id",
            "EmbeddedMaterialDecision.material_id",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].material_id",
            "normalized",
            "material_id",
            "required",
            "Id",
            "EmbeddedDecisionMaterialV1.material_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDecision.path",
            "EmbeddedMaterialDecision.path",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].path",
            "normalized",
            "material_id",
            "required",
            "SourcePath",
            "EmbeddedDecisionMaterialV1.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDecision.sha256",
            "EmbeddedMaterialDecision.sha256",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].sha256",
            "normalized",
            "material_id",
            "required",
            "Hash256",
            "EmbeddedDecisionMaterialV1.sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDecision.disposition",
            "EmbeddedMaterialDecision.disposition",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition",
            "normalized",
            "material_id",
            "required",
            "EmbeddedMaterialDispositionMaterialV1",
            "EmbeddedDecisionMaterialV1.disposition",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::Shipped",
            "EmbeddedMaterialDisposition::Shipped",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.kind",
            "normalized",
            "material_id",
            "required",
            "shipped",
            "EmbeddedMaterialDispositionMaterialV1::Shipped",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::BehaviorOnly",
            "EmbeddedMaterialDisposition::BehaviorOnly",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.kind",
            "normalized",
            "material_id",
            "required",
            "behavior_only",
            "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::Rejected",
            "EmbeddedMaterialDisposition::Rejected",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.kind",
            "normalized",
            "material_id",
            "required",
            "rejected",
            "EmbeddedMaterialDispositionMaterialV1::Rejected",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::Shipped.license",
            "EmbeddedMaterialDisposition::Shipped.license",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=shipped}.value.license",
            "normalized",
            "material_id",
            "required",
            "LicenseDecisionMaterialV1",
            "EmbeddedMaterialDispositionMaterialV1::Shipped.license",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::BehaviorOnly.reason",
            "EmbeddedMaterialDisposition::BehaviorOnly.reason",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=behavior_only}.value.reason",
            "normalized",
            "material_id",
            "required",
            "FindingId",
            "EmbeddedMaterialDispositionMaterialV1::BehaviorOnly.reason",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "EmbeddedMaterialDisposition::Rejected.finding",
            "EmbeddedMaterialDisposition::Rejected.finding",
            Source,
            "provenance",
            "ProvenanceMaterialV1.embedded_material[].disposition{kind=rejected}.value.finding",
            "normalized",
            "material_id",
            "required",
            "FindingId",
            "EmbeddedMaterialDispositionMaterialV1::Rejected.finding",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::canonical_schema_epoch",
            "derived::canonical_schema_epoch",
            Constant,
            "provenance",
            "ProvenanceMaterialV1.canonical_schema_epoch",
            "constant",
            "scalar",
            "required",
            "CANONICAL_SCHEMA_EPOCH",
            "ProvenanceMaterialV1.canonical_schema_epoch",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::classifier_epoch",
            "derived::classifier_epoch",
            Constant,
            "provenance",
            "ProvenanceMaterialV1.classifier_epoch",
            "constant",
            "scalar",
            "required",
            "CLASSIFIER_EPOCH",
            "ProvenanceMaterialV1.classifier_epoch",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::generator_epoch",
            "derived::generator_epoch",
            Constant,
            "provenance",
            "ProvenanceMaterialV1.generator_epoch",
            "constant",
            "scalar",
            "required",
            "GENERATOR_EPOCH",
            "ProvenanceMaterialV1.generator_epoch",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::semantic_sha256",
            "derived::semantic_sha256",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.connector.semantic_sha256",
            "derived:semantic_domain_hash",
            "scalar",
            "required",
            "Hash256",
            "ProvenanceConnectorIdentity.semantic_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::source_identity.record_id",
            "derived::source_identity.record_id",
            NamedDerived,
            "provenance",
            "SourceIdentityMaterialV1.record_id",
            "derived:accepted_record_join",
            "record_id",
            "required",
            "SourceRecordId",
            "SourceIdentityMaterialV1.record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::source_identity.record_sha256",
            "derived::source_identity.record_sha256",
            NamedDerived,
            "provenance",
            "SourceIdentityMaterialV1.record_sha256",
            "derived:source_record_domain_hash",
            "record_id",
            "required",
            "Hash256",
            "SourceIdentityMaterialV1.record_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact.source_record_id",
            "derived::artifact.source_record_id",
            NamedDerived,
            "provenance",
            "ArtifactDecisionMaterialV1.source_record_id",
            "derived:accepted_record_join",
            "source_record_id_then_artifact_id",
            "required",
            "SourceRecordId",
            "ArtifactDecisionMaterialV1.source_record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact.artifact_id",
            "derived::artifact.artifact_id",
            NamedDerived,
            "provenance",
            "ArtifactDecisionMaterialV1.artifact_id",
            "derived:accepted_record_artifact_inventory",
            "source_record_id_then_artifact_id",
            "required",
            "ArtifactId",
            "ArtifactDecisionMaterialV1.artifact_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact.algorithm",
            "derived::artifact.algorithm",
            NamedDerived,
            "provenance",
            "ArtifactDecisionMaterialV1.algorithm",
            "derived:accepted_record_artifact_inventory",
            "source_record_id_then_artifact_id",
            "required",
            "HashAlgorithm",
            "ArtifactDecisionMaterialV1.algorithm",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact.digest",
            "derived::artifact.digest",
            NamedDerived,
            "provenance",
            "ArtifactDecisionMaterialV1.digest",
            "derived:accepted_record_artifact_inventory",
            "source_record_id_then_artifact_id",
            "required",
            "Hash256_or_Hash512",
            "ArtifactDecisionMaterialV1.digest",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact.path",
            "derived::artifact.path",
            NamedDerived,
            "provenance",
            "ArtifactDecisionMaterialV1.path",
            "derived:accepted_record_artifact_inventory",
            "source_record_id_then_artifact_id",
            "explicit_null",
            "Option<SourcePath>",
            "ArtifactDecisionMaterialV1.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::file.source_record_id",
            "derived::file.source_record_id",
            NamedDerived,
            "provenance",
            "FileDecisionMaterialV1.source_record_id",
            "derived:accepted_donat_record_join",
            "source_record_id_then_path",
            "required",
            "SourceRecordId",
            "FileDecisionMaterialV1.source_record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::file.path",
            "derived::file.path",
            NamedDerived,
            "provenance",
            "FileDecisionMaterialV1.path",
            "derived:accepted_donat_file_inventory",
            "source_record_id_then_path",
            "required",
            "RepoPath",
            "FileDecisionMaterialV1.path",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::file.sha256",
            "derived::file.sha256",
            NamedDerived,
            "provenance",
            "FileDecisionMaterialV1.sha256",
            "derived:accepted_donat_file_inventory",
            "source_record_id_then_path",
            "required",
            "Hash256",
            "FileDecisionMaterialV1.sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::source_identity",
            "derived::source_identity",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.sources",
            "derived:accepted_record_join",
            "record_id",
            "empty_array",
            "Vec<SourceIdentityMaterialV1>",
            "ProvenanceMaterialV1.sources",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::artifact",
            "derived::artifact",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.artifacts",
            "derived:accepted_record_artifact_inventory",
            "source_record_id_then_artifact_id",
            "empty_array",
            "Vec<ArtifactDecisionMaterialV1>",
            "ProvenanceMaterialV1.artifacts",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::license",
            "derived::license",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.licenses",
            "derived:accepted_record_license_inventory",
            "canonical_bytes",
            "empty_array",
            "Vec<LicenseDecisionMaterialV1>",
            "ProvenanceMaterialV1.licenses",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::dependency",
            "derived::dependency",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.dependencies",
            "derived:accepted_record_dependency_inventory",
            "dependency",
            "empty_array",
            "Vec<DependencyDecisionMaterialV1>",
            "ProvenanceMaterialV1.dependencies",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::embedded_material",
            "derived::embedded_material",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.embedded_material",
            "derived:accepted_record_embedded_inventory",
            "material_id",
            "empty_array",
            "Vec<EmbeddedDecisionMaterialV1>",
            "ProvenanceMaterialV1.embedded_material",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::notice",
            "derived::notice",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.notices",
            "derived:accepted_record_notice_inventory",
            "id",
            "empty_array",
            "Vec<NoticeMaterialV1>",
            "ProvenanceMaterialV1.notices",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::provider_evidence",
            "derived::provider_evidence",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.provider_evidence",
            "derived:accepted_provider_record_inventory",
            "source_record_id",
            "empty_array",
            "Vec<ProviderEvidenceOriginMaterialV1>",
            "ProvenanceMaterialV1.provider_evidence",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::provider_evidence.source_record_id",
            "derived::provider_evidence.source_record_id",
            NamedDerived,
            "provenance",
            "ProviderEvidenceOriginMaterialV1.source_record_id",
            "derived:accepted_provider_record_join",
            "source_record_id",
            "required",
            "SourceRecordId",
            "ProviderEvidenceOriginMaterialV1.source_record_id",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::fact_origin.artifact_content_sha256",
            "derived::fact_origin.artifact_content_sha256",
            NamedDerived,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.artifact_content_sha256",
            "derived:provider_fact_content_join",
            "use_site",
            "required",
            "Hash256",
            "ResolvedFactOriginV1::ProviderEvidence.artifact_content_sha256",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::fact_origin.location",
            "derived::fact_origin.location",
            NamedDerived,
            "provenance",
            "ResolvedFactOriginMaterialV1{kind=provider_evidence}.value.location",
            "derived:provider_fact_location_join",
            "use_site",
            "required",
            "ExactFactLocationMaterialV1",
            "ResolvedFactOriginV1::ProviderEvidence.location",
            ProjectionSchema,
            Provenance,
            Mutable,
        );

        (
            "derived::donat_policy_ids",
            "derived::donat_policy_ids",
            NamedDerived,
            "provenance",
            "ProvenanceMaterialV1.donat_policy_ids",
            "derived:contract_fact_policy_set",
            "lexical",
            "empty_array",
            "Vec<DonatPolicyId>",
            "ProvenanceMaterialV1.donat_policy_ids",
            ProjectionSchema,
            Provenance,
            Mutable,
        );
    }
    context struct ProvenanceProjectionContext
        using validate_provenance_projection_context {
        parameters {
            canonical_schema_epoch: u32;
            classifier_epoch: u32;
            generator_epoch: u32;
        }
        derived_fields {
            semantic_sha256: String;
            referenced_ids: Vec<SourceRecordId>;
            fact_origins: BTreeMap<String, ValidatedResolvedFactOriginContext>;
            donat_policy_ids: Vec<String>;
        }
        validate(checked, manifest) {
            if canonical_schema_epoch == 0
                || classifier_epoch == 0
                || generator_epoch == 0
            {
                return Err(CatalogError::new(
                    "catalog_projection_input_mismatch",
                    "provenance epochs must be nonzero",
                ));
            }
            let accepted_records = checked.accepted_records();
            let reviewed_policies = checked.reviewed_policies();
            let semantic = semantic_material(
                checked,
                canonical_schema_epoch,
            )?;
            let semantic_sha256 =
                hex_bytes(semantic_sha256(&semantic)?.as_bytes());
            let referenced_ids = manifest
                .provenance
                .iter()
                .map(|reference| reference.source_record_id)
                .collect::<BTreeSet<_>>();
            if referenced_ids.len() != manifest.provenance.len()
                || referenced_ids.is_empty()
            {
                return projection_input_mismatch(
                    "provenance source references are empty or duplicate",
                );
            }
            for record_id in &referenced_ids {
                accepted_records
                    .capability_record(*record_id)
                    .ok_or_else(|| {
                        CatalogError::new(
                            "catalog_projection_input_mismatch",
                            "provenance source record has no checked capability",
                        )
                    })?;
            }
            let semantic_values = manifest
                .operations
                .iter()
                .flat_map(|operation| {
                    operation.resolved_fact_values.iter()
                })
                .cloned()
                .collect::<Vec<_>>();
            let fact_bindings = manifest
                .provenance
                .iter()
                .flat_map(|reference| {
                    reference.contract_facts.iter()
                })
                .map(|binding| ResolvedContractFactBinding {
                    use_site: binding.use_site.clone(),
                    fact: binding.fact.clone(),
                })
                .collect::<Vec<_>>();
            let (_, resolved_origins) =
                resolve_fact_binding_contexts(
                    &semantic_values,
                    &fact_bindings,
                    checked.fact_requirements(),
                    accepted_records,
                    reviewed_policies,
                )?;
            let fact_origins = resolved_origins
                .into_iter()
                .map(|origin| (origin.use_site.clone(), origin))
                .collect::<BTreeMap<_, _>>();
            let mut donat_policy_ids = fact_bindings
                .iter()
                .filter_map(|binding| match &binding.fact {
                    ContractFact::DonatPolicy {
                        policy_id,
                        value: _,
                    } => Some(policy_id.as_str().to_owned()),
                    ContractFact::ProviderEvidence {
                        source_record_id: _,
                        fact_id: _,
                    } => None,
                })
                .collect::<Vec<_>>();
            donat_policy_ids.sort();
            donat_policy_ids.dedup();
            Ok(ProvenanceProjectionContext {
                checked,
                canonical_schema_epoch,
                classifier_epoch,
                generator_epoch,
                semantic_sha256,
                referenced_ids:
                    referenced_ids.into_iter().collect(),
                fact_origins,
                donat_policy_ids,
            })
        }
    }
    normalized_contexts {
        struct ValidatedResolvedFactOriginContext {
            use_site: String;
            origin: ValidatedResolvedFactOrigin;
        }
        enum ValidatedResolvedFactOrigin {
            ProviderEvidence {
                source_record_id: SourceRecordId;
                fact_id: crate::ProviderFactId;
                artifact_content_sha256: crate::Hash256;
                location: ExactFactLocation;
            }
            DonatPolicy {
                policy_id: DonatPolicyId;
            }
        }
    }
    root pub struct ProvenanceMaterialV1
        using provenance_material(checked, context) {
        (
            artifacts: Vec<ArtifactDecisionMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = Vec::new();
                for record_id in &context.referenced_ids {
                    let record = accepted_records
                        .capability_record(*record_id)
                        .expect(
                            "validated provenance record remains accepted",
                        );
                    for artifact in &record.artifact_hashes {
                        values.push(artifact_decision_material(
                            record.record_id,
                            artifact,
                        )?);
                    }
                }
                values.sort_by(|left, right| {
                    (&left.source_record_id, &left.artifact_id).cmp(&(
                        &right.source_record_id,
                        &right.artifact_id,
                    ))
                });
                values
            },
        );
        (
            canonical_schema_epoch: u32 = {
                context.canonical_schema_epoch
            },
        );
        (
            classifier_epoch: u32 = {
                context.classifier_epoch
            },
        );
        (
            connector: ProvenanceConnectorIdentity = {
                provenance_connector_identity(
                    context.checked.manifest(),
                    &context.semantic_sha256,
                )?
            },
        );
        (
            dependencies: Vec<DependencyDecisionMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = Vec::new();
                for record_id in &context.referenced_ids {
                    let record = accepted_records
                        .capability_record(*record_id)
                        .expect(
                            "validated provenance record remains accepted",
                        );
                    values.extend(
                        record
                            .dependencies
                            .iter()
                            .map(dependency_decision_material)
                            .collect::<Result<Vec<_>, CatalogError>>()?,
                    );
                }
                values.sort_by(|left, right| {
                    left.dependency.cmp(&right.dependency)
                });
                values
            },
        );
        (
            donat_policy_ids: Vec<String> = {
                context.donat_policy_ids.clone()
            },
        );
        (
            embedded_material: Vec<EmbeddedDecisionMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = Vec::new();
                for record_id in &context.referenced_ids {
                    let record = accepted_records
                        .capability_record(*record_id)
                        .expect(
                            "validated provenance record remains accepted",
                        );
                    values.extend(
                        record
                            .embedded_material
                            .iter()
                            .map(embedded_decision_material)
                            .collect::<Result<Vec<_>, CatalogError>>()?,
                    );
                }
                values.sort_by(|left, right| {
                    left.material_id.cmp(&right.material_id)
                });
                values
            },
        );
        (
            files: Vec<FileDecisionMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = Vec::new();
                for record_id in &context.referenced_ids {
                    let record = accepted_records
                        .capability_record(*record_id)
                        .expect(
                            "validated provenance record remains accepted",
                        );
                    if let SourceSubject::DonatOwned(source) =
                        &record.subject
                    {
                        for file in &source.files {
                            values.push(file_decision_material(
                                record.record_id,
                                file,
                            )?);
                        }
                    }
                }
                values.sort_by(|left, right| {
                    (&left.source_record_id, &left.path).cmp(&(
                        &right.source_record_id,
                        &right.path,
                    ))
                });
                values
            },
        );
        (
            generator_epoch: u32 = {
                context.generator_epoch
            },
        );
        (
            licenses: Vec<LicenseDecisionMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = context
                    .referenced_ids
                    .iter()
                    .map(|record_id| {
                        let record = accepted_records
                            .capability_record(*record_id)
                            .expect(
                                "validated provenance record remains accepted",
                            );
                        license_material(&record.license)
                    })
                    .collect::<Result<Vec<_>, CatalogError>>()?;
                sort_and_deduplicate_materials(&mut values)?;
                values
            },
        );
        (
            manifest_references: Vec<ManifestProvenanceMaterialV1> = {
                let mut values = context
                    .checked
                    .manifest()
                    .provenance
                    .iter()
                    .map(|reference| {
                        manifest_provenance_material(
                            reference,
                            &context.fact_origins,
                        )
                    })
                    .collect::<Result<Vec<_>, CatalogError>>()?;
                values.sort_by(|left, right| {
                    left.source_record_id
                        .cmp(&right.source_record_id)
                });
                values
            },
        );
        (
            notices: Vec<NoticeMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = context
                    .referenced_ids
                    .iter()
                    .map(|record_id| {
                        let record = accepted_records
                            .capability_record(*record_id)
                            .expect(
                                "validated provenance record remains accepted",
                            );
                        notice_material(&record.notice)
                    })
                    .collect::<Result<Vec<_>, CatalogError>>()?;
                values.sort_by(|left, right| {
                    left.id.cmp(&right.id)
                });
                values.dedup_by(|left, right| left == right);
                values
            },
        );
        (
            provider_evidence:
                Vec<ProviderEvidenceOriginMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = Vec::new();
                for record_id in &context.referenced_ids {
                    let record = accepted_records
                        .capability_record(*record_id)
                        .expect(
                            "validated provenance record remains accepted",
                        );
                    if let SourceSubject::ProviderArtifact(provider) =
                        &record.subject
                    {
                        values.push(
                            provider_evidence_origin_material(
                                record.record_id,
                                provider,
                            )?,
                        );
                    }
                }
                values.sort_by(|left, right| {
                    left.source_record_id
                        .cmp(&right.source_record_id)
                });
                values
            },
        );
        (
            sources: Vec<SourceIdentityMaterialV1> = {
                let accepted_records =
                    context.checked.accepted_records();
                let mut values = context
                    .referenced_ids
                    .iter()
                    .map(|record_id| {
                        let record = accepted_records
                            .capability_record(*record_id)
                            .expect(
                                "validated provenance record remains accepted",
                            );
                        source_identity_material(record)
                    })
                    .collect::<Result<Vec<_>, CatalogError>>()?;
                values.sort_by(|left, right| {
                    left.record_id.cmp(&right.record_id)
                });
                values
            },
        );
    }
    structs {
        struct SourceIdentityMaterialV1
            using source_identity_material(
                record: &ConnectorSourceRecord,
            ) {
            (
                record_id: String = {
                    record.record_id.as_str().to_owned()
                },
            );
            (
                record_sha256: String = {
                    let material = source_record_material(record)?;
                    hex_bytes(record_sha256(&material)?.as_bytes())
                },
            );
        }
        struct ArtifactDecisionMaterialV1
            using artifact_decision_material(
                source_record_id: SourceRecordId,
                artifact: &ArtifactHash,
            ) {
            (
                source_record_id: String = {
                    source_record_id.as_str().to_owned()
                },
            );
            (
                artifact_id: String = {
                    artifact.artifact_id.to_string()
                },
            );
            (
                algorithm: HashAlgorithmMaterialV1 = {
                    hash_algorithm_material(&artifact.algorithm)?
                },
            );
            (
                digest: String = {
                    artifact.digest.clone()
                },
            );
            (
                path: Option<String> = {
                    artifact.path.as_ref().map(ToString::to_string)
                },
            );
        }
        struct FileDecisionMaterialV1
            using file_decision_material(
                source_record_id: SourceRecordId,
                file: &crate::RepoFileHash,
            ) {
            (
                source_record_id: String = {
                    source_record_id.as_str().to_owned()
                },
            );
            (
                path: String = {
                    file.path.to_string()
                },
            );
            (
                sha256: String = {
                    file.sha256.to_string()
                },
            );
        }
        struct ManifestProvenanceMaterialV1
            using manifest_provenance_material(
                reference: &crate::ManifestProvenanceReference,
                origins_by_site:
                    &BTreeMap<
                        String,
                        ValidatedResolvedFactOriginContext,
                    >,
            ) {
            (
                source_record_id: String = {
                    reference.source_record_id.as_str().to_owned()
                },
            );
            (
                artifact_hashes: Vec<ArtifactHashMaterialV1> = {
                    let mut values = reference
                        .artifact_hashes
                        .iter()
                        .map(artifact_hash_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        artifact_hash_material_key(left)
                            .cmp(artifact_hash_material_key(right))
                    });
                    values
                },
            );
            (
                license_id: String = {
                    reference.license_id.clone()
                },
            );
            (
                notice_id: String = {
                    reference.notice_id.as_str().to_owned()
                },
            );
            (
                #[serde(
                    deserialize_with =
                        "deserialize_resolved_fact_origins"
                )]
                contract_fact_origins:
                    Vec<ResolvedFactOriginMaterialV1> = {
                    let mut values = reference
                        .contract_facts
                        .iter()
                        .map(|binding| {
                            let origin = origins_by_site
                                .get(&binding.use_site)
                                .ok_or_else(|| {
                                    CatalogError::new(
                                        "catalog_projection_input_mismatch",
                                        "manifest provenance fact has no resolved origin",
                                    )
                                })?;
                            resolved_fact_origin_material(origin)
                        })
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        left.use_site.cmp(&right.use_site)
                    });
                    values
                },
            );
        }
        struct ProviderEvidenceOriginMaterialV1
            using provider_evidence_origin_material(
                source_record_id: SourceRecordId,
                provider: &crate::ExactProviderArtifact,
            ) {
            (
                source_record_id: String = {
                    source_record_id.as_str().to_owned()
                },
            );
            (
                provider: String = {
                    provider.provider.clone()
                },
            );
            (
                evidence:
                    Vec<ProviderEvidenceOriginEntryMaterialV1> = {
                    let mut values = provider
                        .evidence
                        .iter()
                        .map(provider_evidence_origin_entry_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        provider_evidence_source_key(&left.source)
                            .cmp(&provider_evidence_source_key(
                                &right.source,
                            ))
                            .then_with(|| {
                                left.content_sha256
                                    .cmp(&right.content_sha256)
                            })
                    });
                    values
                },
            );
        }
        struct ProviderEvidenceOriginEntryMaterialV1
            using provider_evidence_origin_entry_material(
                entry: &crate::ProviderEvidenceArtifact,
            ) {
            (
                source: ProviderEvidenceSourceMaterialV1 = {
                    provider_evidence_source_material(
                        &entry.source,
                    )?
                },
            );
            (
                accessed_on: String = {
                    entry.accessed_on.to_string()
                },
            );
            (
                content_sha256: String = {
                    entry.content_sha256.to_string()
                },
            );
            (
                terms: EvidenceTermsMaterialV1 = {
                    evidence_terms_material(&entry.terms)?
                },
            );
            (
                facts:
                    Vec<ProviderEvidenceOriginFactMaterialV1> = {
                    let mut values = entry
                        .facts
                        .iter()
                        .map(provider_evidence_origin_fact_material)
                        .collect::<Result<Vec<_>, CatalogError>>()?;
                    values.sort_by(|left, right| {
                        left.fact_id.cmp(&right.fact_id)
                    });
                    values
                },
            );
        }
        struct ProviderEvidenceOriginFactMaterialV1
            using provider_evidence_origin_fact_material(
                fact: &crate::ProviderFact,
            ) {
            (
                fact_id: String = {
                    fact.fact_id.as_str().to_owned()
                },
            );
            (
                location: ExactFactLocationMaterialV1 = {
                    fact_location_material(&fact.location)?
                },
            );
        }
        pub struct ProvenanceConnectorIdentity
            using provenance_connector_identity(
                manifest: &crate::ConnectorManifest,
                semantic_sha256: &str,
            ) {
            (
                id: String = {
                    manifest.connector.as_str().to_owned()
                },
            );
            (
                semantic_sha256: String = {
                    semantic_sha256.to_owned()
                },
            );
            (
                version: StableSemver = {
                    manifest.connector_version
                },
            );
        }
        pub struct ResolvedFactOriginMaterialV1
            using resolved_fact_origin_material(
                value: &ValidatedResolvedFactOriginContext,
            ) {
            (
                use_site: String = {
                    value.use_site.clone()
                },
            );
            (
                origin: ResolvedFactOriginV1 = {
                    resolved_fact_origin(&value.origin)?
                },
            );
        }
    }
    tagged_enums {
        enum ResolvedFactOriginV1
            from (ValidatedResolvedFactOrigin)
            using resolved_fact_origin(value) {
            ProviderEvidence as "provider_evidence" {
                (
                    source_record_id: String = {
                        source_record_id.as_str().to_owned()
                    },
                );
                (
                    fact_id: String = {
                        fact_id.as_str().to_owned()
                    },
                );
                (
                    artifact_content_sha256: String = {
                        artifact_content_sha256.to_string()
                    },
                );
                (
                    location: ExactFactLocationMaterialV1 = {
                        fact_location_material(location)?
                    },
                );
            }
            DonatPolicy as "donat_policy" {
                (
                    policy_id: String = {
                        policy_id.as_str().to_owned()
                    },
                );
            }
        }
    }
    derived_dependencies {
        (
            "canonical_schema_epoch",
            "ProvenanceMaterialV1.canonical_schema_epoch",
            "constant_parameter",
        );
        (
            "canonical_schema_epoch",
            "ProvenanceConnectorIdentity.semantic_sha256",
            "semantic_domain_hash",
        );
        (
            "classifier_epoch",
            "ProvenanceMaterialV1.classifier_epoch",
            "constant_parameter",
        );
        (
            "generator_epoch",
            "ProvenanceMaterialV1.generator_epoch",
            "constant_parameter",
        );
        (
            "accepted_source_record",
            "SourceIdentityMaterialV1.record_sha256",
            "source_record_domain_hash",
        );
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConnectorManifestDto {
    connector: String,
    connector_version: StableSemver,
    manifest_version: u32,
    runtime_abi_epoch: u32,
    value_language_epoch: u32,
    provider: String,
    api_identity: String,
    credentials: Vec<SemanticCredentialMaterialV1>,
    origins: Vec<SemanticOriginMaterialV1>,
    operations: Vec<SemanticOperationMaterialV1>,
    triggers: Vec<SemanticTriggerMaterialV1>,
    provenance: Vec<ManifestProvenanceInputDto>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestProvenanceInputDto {
    #[serde(deserialize_with = "crate::source::deserialize_source_primitive")]
    source_record_id: SourceRecordId,
    #[serde(deserialize_with = "crate::source::deserialize_artifact_hashes")]
    artifact_hashes: Vec<ArtifactHash>,
    license_id: String,
    #[serde(deserialize_with = "crate::source::deserialize_source_primitive")]
    notice_id: crate::NoticeId,
    contract_facts: Vec<ResolvedContractFactBindingDto>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResolvedContractFactBindingDto {
    use_site: String,
    #[serde(deserialize_with = "crate::source::deserialize_contract_fact")]
    fact: ContractFact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ValueContractMaterialDto {
    named_objects: BTreeMap<String, NamedObjectMaterialV1>,
    roots: BTreeMap<String, FieldMaterialV1>,
    value_language_epoch: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedFactValueMaterialDto {
    use_site: String,
    #[serde(deserialize_with = "crate::source::deserialize_typed_value_material")]
    value: TypedValueMaterialV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedFactOriginMaterialDto {
    use_site: String,
    origin: ResolvedFactOriginV1,
}

}

fn deserialize_value_contract_material<'de, D>(
    deserializer: D,
) -> Result<ValueContractMaterialV1, D::Error>
where
    D: Deserializer<'de>,
{
    value_contract_material_from_dto(ValueContractMaterialDto::deserialize(deserializer)?)
        .map_err(serde::de::Error::custom)
}

fn deserialize_resolved_fact_values<'de, D>(
    deserializer: D,
) -> Result<Vec<ResolvedFactValueMaterialV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<ResolvedFactValueMaterialDto>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| {
            validate_material_name(&value.use_site).map_err(serde::de::Error::custom)?;
            Ok(ResolvedFactValueMaterialV1 {
                use_site: value.use_site,
                value: value.value,
            })
        })
        .collect()
}

fn deserialize_resolved_fact_origins<'de, D>(
    deserializer: D,
) -> Result<Vec<ResolvedFactOriginMaterialV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<ResolvedFactOriginMaterialDto>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| {
            validate_material_name(&value.use_site).map_err(serde::de::Error::custom)?;
            Ok(ResolvedFactOriginMaterialV1 {
                use_site: value.use_site,
                origin: value.origin,
            })
        })
        .collect()
}

/// Decode the repository-owned normalized connector manifest.
///
/// This loader is deliberately separate from canonical-material decoding:
/// canonical materials are output-only hash inputs, while this DTO is the
/// complete reviewed input to the offline compiler.
pub fn load_connector_manifest_bytes(bytes: &[u8]) -> Result<ConnectorManifest, CatalogError> {
    let parsed_value = serde_yaml::from_slice::<serde_yaml::Value>(bytes)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let dto = serde_yaml::from_slice::<ConnectorManifestDto>(bytes)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let parsed_json = serde_json::to_vec(&parsed_value)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    let rebuilt_json = serde_json::to_vec(&dto)
        .map_err(|error| CatalogError::new("catalog_manifest_incomplete", error.to_string()))?;
    if canonicalize_raw(&parsed_json)? != canonicalize_raw(&rebuilt_json)? {
        return Err(CatalogError::new(
            "catalog_manifest_incomplete",
            "normalized manifest omitted or changed a required member",
        ));
    }
    connector_manifest_from_dto(dto)
}

fn invalid_manifest_primitive(detail: impl Into<String>) -> CatalogError {
    CatalogError::new("catalog_manifest_invalid_primitive", detail)
}

fn checked_manifest_primitive<T, E>(
    value: Result<T, E>,
    detail: &'static str,
) -> Result<T, CatalogError> {
    value.map_err(|_| invalid_manifest_primitive(detail))
}

fn nonzero_u16(value: u16, detail: &'static str) -> Result<NonZeroU16, CatalogError> {
    NonZeroU16::new(value).ok_or_else(|| invalid_manifest_primitive(detail))
}

fn nonzero_u32(value: u32, detail: &'static str) -> Result<NonZeroU32, CatalogError> {
    NonZeroU32::new(value).ok_or_else(|| invalid_manifest_primitive(detail))
}

fn nonzero_u64_text(value: &str, detail: &'static str) -> Result<NonZeroU64, CatalogError> {
    let parsed = value
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .and_then(NonZeroU64::new);
    parsed.ok_or_else(|| invalid_manifest_primitive(detail))
}

fn u64_text(value: &str, detail: &'static str) -> Result<u64, CatalogError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
        .ok_or_else(|| invalid_manifest_primitive(detail))
}

fn hash256_text(value: &str, detail: &'static str) -> Result<[u8; 32], CatalogError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_manifest_primitive(detail));
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(pair[0]).ok_or_else(|| invalid_manifest_primitive(detail))?;
        let low = hex_value(pair[1]).ok_or_else(|| invalid_manifest_primitive(detail))?;
        output[index] = high << 4 | low;
    }
    if hex_bytes(&output) != value {
        return Err(invalid_manifest_primitive(detail));
    }
    Ok(output)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn value_contract_from_material(value: ValueContractMaterialV1) -> ValueContractCatalog {
    fn field(value: FieldMaterialV1) -> ValueContractField {
        ValueContractField {
            required: value.required,
            type_ref: type_ref(value.type_ref),
        }
    }

    fn type_ref(value: TypeRefMaterialV1) -> TypeRef {
        TypeRef {
            nullable: value.nullable,
            value_type: value_type(value.value_type),
        }
    }

    fn value_type(value: ValueTypeMaterialV1) -> ValueType {
        match value.0 {
            ValueTypeMaterial::Scalar(value) => ValueType::Scalar {
                scalar: match value.0 {
                    ValueScalarMaterial::Boolean(()) => ValueScalar::Boolean,
                    ValueScalarMaterial::String(()) => ValueScalar::String,
                    ValueScalarMaterial::Int32(()) => ValueScalar::Int32,
                    ValueScalarMaterial::Int64(()) => ValueScalar::Int64,
                    ValueScalarMaterial::UInt64(()) => ValueScalar::UInt64,
                    ValueScalarMaterial::Decimal(()) => ValueScalar::Decimal,
                    ValueScalarMaterial::Uuid(()) => ValueScalar::Uuid,
                    ValueScalarMaterial::Date(()) => ValueScalar::Date,
                    ValueScalarMaterial::Timestamp(()) => ValueScalar::Timestamp,
                    ValueScalarMaterial::TimestampTz(()) => ValueScalar::TimestampTz,
                    ValueScalarMaterial::Json(()) => ValueScalar::Json,
                    ValueScalarMaterial::Custom { name } => ValueScalar::Custom { name },
                },
            },
            ValueTypeMaterial::Enum { name, values } => ValueType::Enum { name, values },
            ValueTypeMaterial::Object { fields } => ValueType::Object {
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, field(value)))
                    .collect(),
            },
            ValueTypeMaterial::List { element } => ValueType::List {
                element: Box::new(type_ref(*element)),
            },
            ValueTypeMaterial::Ref { name } => ValueType::Ref { name },
        }
    }

    ValueContractCatalog {
        roots: value
            .roots
            .into_iter()
            .map(|(name, value)| (name, field(value)))
            .collect(),
        named_objects: value
            .named_objects
            .into_iter()
            .map(|(name, object)| {
                (
                    name,
                    ValueObjectContract {
                        fields: object
                            .fields
                            .into_iter()
                            .map(|(name, value)| (name, field(value)))
                            .collect(),
                    },
                )
            })
            .collect(),
    }
}

fn typed_value_from_material(value: TypedValueMaterialV1) -> Result<TypedValue, CatalogError> {
    value
        .to_typed_value()
        .map_err(|_| invalid_manifest_primitive("typed value"))
}

fn connector_manifest_from_dto(
    value: ConnectorManifestDto,
) -> Result<ConnectorManifest, CatalogError> {
    let ConnectorManifestDto {
        connector,
        connector_version,
        manifest_version,
        runtime_abi_epoch,
        value_language_epoch,
        provider,
        api_identity,
        credentials,
        origins,
        operations,
        triggers,
        provenance,
    } = value;
    Ok(ConnectorManifest {
        connector: checked_manifest_primitive(
            ConnectorId::parse(&connector),
            "connector identity",
        )?,
        connector_version,
        manifest_version,
        runtime_abi_epoch,
        value_language_epoch,
        provider,
        api_identity,
        credentials: credentials
            .into_iter()
            .map(credential_from_material)
            .collect::<Result<_, _>>()?,
        origins: origins
            .into_iter()
            .map(origin_from_material)
            .collect::<Result<_, _>>()?,
        operations: operations
            .into_iter()
            .map(operation_from_material)
            .collect::<Result<_, _>>()?,
        triggers: triggers
            .into_iter()
            .map(trigger_from_material)
            .collect::<Result<_, _>>()?,
        provenance: provenance
            .into_iter()
            .map(|reference| ManifestProvenanceReference {
                source_record_id: reference.source_record_id,
                artifact_hashes: reference.artifact_hashes,
                license_id: reference.license_id,
                notice_id: reference.notice_id,
                contract_facts: reference
                    .contract_facts
                    .into_iter()
                    .map(|binding| ResolvedContractFactBinding {
                        use_site: binding.use_site,
                        fact: binding.fact,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn credential_from_material(
    value: SemanticCredentialMaterialV1,
) -> Result<CredentialSpec, CatalogError> {
    let SemanticCredentialMaterialV1 {
        credential,
        version,
        fields,
        auth_plan,
        allowed_origins,
        scopes,
        auth_processor,
        credential_test_operation,
        bounds,
    } = value;
    Ok(CredentialSpec {
        credential: checked_manifest_primitive(
            CredentialSpecId::parse(&credential),
            "credential identity",
        )?,
        version,
        fields: fields
            .into_iter()
            .map(credential_field_from_material)
            .collect::<Result<_, _>>()?,
        auth_plan: auth_plan_from_material(auth_plan)?,
        allowed_origins: allowed_origins
            .into_iter()
            .map(|origin| {
                checked_manifest_primitive(OriginId::parse(&origin), "allowed origin identity")
            })
            .collect::<Result<_, _>>()?,
        scopes,
        auth_processor: auth_processor
            .map(authenticator_ref_from_material)
            .transpose()?,
        credential_test_operation: credential_test_operation
            .map(|reference| {
                Ok(VersionedOperationReference {
                    operation: checked_manifest_primitive(
                        OperationId::parse(&reference.operation),
                        "credential-test operation identity",
                    )?,
                    version: reference.version,
                })
            })
            .transpose()?,
        bounds: CredentialBounds {
            maximum_field_bytes: nonzero_u32(
                bounds.maximum_field_bytes,
                "maximum credential field bytes",
            )?,
            maximum_aggregate_bytes: nonzero_u32(
                bounds.maximum_aggregate_bytes,
                "maximum aggregate credential bytes",
            )?,
            maximum_token_bytes: nonzero_u32(
                bounds.maximum_token_bytes,
                "maximum credential token bytes",
            )?,
        },
    })
}

fn credential_field_from_material(
    value: CredentialFieldMaterialV1,
) -> Result<CredentialFieldSpec, CatalogError> {
    Ok(CredentialFieldSpec {
        field: checked_manifest_primitive(
            CredentialFieldId::parse(&value.field),
            "credential field identity",
        )?,
        required: value.required,
        secret: match value.secret {
            SecretClassificationMaterialV1::Secret(()) => SecretClassification::Secret,
            SecretClassificationMaterialV1::Sensitive(()) => SecretClassification::Sensitive,
            SecretClassificationMaterialV1::NonSecret(()) => SecretClassification::NonSecret,
        },
        maximum_bytes: nonzero_u32(value.maximum_bytes, "maximum credential field bytes")?,
        redaction: redaction_from_material(value.redaction),
    })
}

fn redaction_from_material(value: RedactionMaterialV1) -> RedactionPlan {
    match value {
        RedactionMaterialV1::Omit(()) => RedactionPlan::Omit,
        RedactionMaterialV1::Fixed { replacement } => RedactionPlan::Fixed { replacement },
        RedactionMaterialV1::PreserveLast { characters } => {
            RedactionPlan::PreserveLast { characters }
        }
    }
}

fn auth_plan_from_material(value: CredentialAuthMaterialV1) -> Result<AuthPlan, CatalogError> {
    Ok(match value {
        CredentialAuthMaterialV1::FixedHeaderApiKey { field, header } => {
            AuthPlan::FixedHeaderApiKey {
                field: checked_manifest_primitive(
                    CredentialFieldId::parse(&field),
                    "auth field identity",
                )?,
                header,
            }
        }
        CredentialAuthMaterialV1::FixedQueryApiKey { field, query } => AuthPlan::FixedQueryApiKey {
            field: checked_manifest_primitive(
                CredentialFieldId::parse(&field),
                "auth field identity",
            )?,
            query,
        },
        CredentialAuthMaterialV1::Bearer { token } => AuthPlan::Bearer {
            token: checked_manifest_primitive(
                CredentialFieldId::parse(&token),
                "auth token identity",
            )?,
        },
        CredentialAuthMaterialV1::HttpBasic { username, password } => AuthPlan::HttpBasic {
            username: checked_manifest_primitive(
                CredentialFieldId::parse(&username),
                "auth username identity",
            )?,
            password: checked_manifest_primitive(
                CredentialFieldId::parse(&password),
                "auth password identity",
            )?,
        },
        CredentialAuthMaterialV1::OAuth2ClientCredentials {
            client_id,
            client_secret,
            token_origin,
            token_step,
            scopes,
            token_pointer,
        } => AuthPlan::OAuth2ClientCredentials {
            client_id: checked_manifest_primitive(
                CredentialFieldId::parse(&client_id),
                "OAuth client identity",
            )?,
            client_secret: checked_manifest_primitive(
                CredentialFieldId::parse(&client_secret),
                "OAuth secret identity",
            )?,
            token_origin: checked_manifest_primitive(
                OriginId::parse(&token_origin),
                "OAuth token origin",
            )?,
            token_step: checked_manifest_primitive(
                CompiledStepId::parse(&token_step),
                "OAuth token step",
            )?,
            scopes,
            token_pointer,
        },
        CredentialAuthMaterialV1::PreprovisionedOAuthAccessToken { token } => {
            AuthPlan::PreprovisionedOAuthAccessToken {
                token: checked_manifest_primitive(
                    CredentialFieldId::parse(&token),
                    "OAuth access-token identity",
                )?,
            }
        }
    })
}

fn origin_from_material(value: SemanticOriginMaterialV1) -> Result<FixedOrigin, CatalogError> {
    let HttpsMaterialV1::HttpsOnly(()) = value.scheme;
    Ok(FixedOrigin {
        origin: checked_manifest_primitive(OriginId::parse(&value.origin), "origin identity")?,
        scheme: HttpsOnly,
        host: value.host,
        port: nonzero_u16(value.port, "origin port")?,
        network_policy: match value.network_policy {
            NetworkPolicyMaterialV1::PublicOnly(()) => NetworkPolicy::PublicOnly,
            NetworkPolicyMaterialV1::PrivateAllowed { policy } => {
                NetworkPolicy::PrivateAllowed { policy }
            }
        },
    })
}

fn authenticator_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<AuthenticatorId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(
            AuthenticatorId::parse(&value.id),
            "authenticator identity",
        )?,
        implementation_revision: value.implementation_revision,
    })
}

fn codec_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<CodecId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(CodecId::parse(&value.id), "codec identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn normalizer_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<NormalizerId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(NormalizerId::parse(&value.id), "normalizer identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn processor_ref_from_material(
    value: VersionedProcessorMaterialV1,
) -> Result<VersionedProcessorRef<ProcessorFamilyId>, CatalogError> {
    Ok(VersionedProcessorRef {
        id: checked_manifest_primitive(ProcessorFamilyId::parse(&value.id), "processor identity")?,
        implementation_revision: value.implementation_revision,
    })
}

fn operation_from_material(
    value: SemanticOperationMaterialV1,
) -> Result<OperationSpec, CatalogError> {
    let SemanticOperationMaterialV1 {
        connector,
        connector_version,
        operation,
        operation_version,
        runtime_abi_epoch,
        value_language_epoch,
        input,
        input_contract_sha256,
        output,
        output_contract_sha256,
        credential,
        origins,
        steps,
        pre_request_transforms,
        post_response_transforms,
        operation_processor,
        effect,
        pagination,
        error_map,
        capacity,
        rate,
        serialization_key_default,
        bounds,
        resolved_fact_values,
    } = value;
    Ok(OperationSpec {
        connector: checked_manifest_primitive(
            ConnectorId::parse(&connector),
            "operation connector identity",
        )?,
        connector_version,
        operation: checked_manifest_primitive(
            OperationId::parse(&operation),
            "operation identity",
        )?,
        operation_version,
        runtime_abi_epoch,
        value_language_epoch,
        input: value_contract_from_material(input),
        input_contract_sha256: hash256_text(&input_contract_sha256, "input contract hash")?,
        output: value_contract_from_material(output),
        output_contract_sha256: hash256_text(&output_contract_sha256, "output contract hash")?,
        credential: credential
            .map(|reference| {
                Ok(VersionedCredentialReference {
                    credential: checked_manifest_primitive(
                        CredentialSpecId::parse(&reference.credential),
                        "operation credential identity",
                    )?,
                    version: reference.version,
                })
            })
            .transpose()?,
        origins: origins
            .into_iter()
            .map(origin_from_material)
            .collect::<Result<_, _>>()?,
        steps: steps
            .into_iter()
            .map(step_from_material)
            .collect::<Result<_, _>>()?,
        pre_request_transforms: pre_request_transforms
            .into_iter()
            .map(processor_ref_from_material)
            .collect::<Result<_, _>>()?,
        post_response_transforms: post_response_transforms
            .into_iter()
            .map(processor_ref_from_material)
            .collect::<Result<_, _>>()?,
        operation_processor: operation_processor
            .map(processor_ref_from_material)
            .transpose()?,
        effect: effect_from_material(effect)?,
        pagination: pagination_from_material(pagination)?,
        error_map: error_map_from_material(error_map)?,
        capacity: CapacityDefaults {
            maximum_in_flight: nonzero_u32(
                capacity.maximum_in_flight,
                "maximum in-flight operations",
            )?,
        },
        rate: RateDefaults {
            burst: nonzero_u32(rate.burst, "rate burst")?,
            refill_interval_ms: nonzero_u64_text(&rate.refill_interval_ms, "rate refill interval")?,
        },
        serialization_key_default: serialization_key_default
            .map(|value| {
                Ok(TypedSerializationKeyDefault {
                    field: value.field,
                    value: typed_value_from_material(value.value)?,
                })
            })
            .transpose()?,
        bounds: operation_bounds_from_material(bounds)?,
        resolved_fact_values: resolved_fact_values
            .into_iter()
            .map(|binding| {
                Ok(ResolvedFactValue {
                    use_site: binding.use_site,
                    value: typed_value_from_material(binding.value)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
    })
}

fn step_from_material(value: SemanticStepMaterialV1) -> Result<CompiledStepSpec, CatalogError> {
    Ok(CompiledStepSpec {
        step: checked_manifest_primitive(CompiledStepId::parse(&value.step), "step identity")?,
        method: value.method,
        origin: checked_manifest_primitive(OriginId::parse(&value.origin), "step origin")?,
        path: value.path,
        query: value
            .query
            .into_iter()
            .map(|binding| {
                Ok(CompiledQueryBinding {
                    name: binding.name,
                    binding: binding_from_material(binding.binding)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        headers: value
            .headers
            .into_iter()
            .map(|binding| {
                Ok(CompiledHeaderBinding {
                    name: binding.name,
                    binding: binding_from_material(binding.binding)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        credential_action: value
            .credential_action
            .map(|action| {
                Ok(CompiledCredentialAction {
                    credential: checked_manifest_primitive(
                        CredentialSpecId::parse(&action.credential),
                        "step credential identity",
                    )?,
                })
            })
            .transpose()?,
        request: match value.request {
            CompiledRequestMaterialV1::None(()) => CompiledRequestShape::None,
            CompiledRequestMaterialV1::Json { bindings } => CompiledRequestShape::Json { bindings },
            CompiledRequestMaterialV1::FormUrlencoded { bindings } => {
                CompiledRequestShape::FormUrlencoded { bindings }
            }
            CompiledRequestMaterialV1::Multipart { bindings } => {
                CompiledRequestShape::Multipart { bindings }
            }
            CompiledRequestMaterialV1::RawBytes { binding } => {
                CompiledRequestShape::RawBytes { binding }
            }
        },
        success_statuses: value
            .success_statuses
            .into_iter()
            .map(|range| StatusRange {
                minimum: range.minimum,
                maximum: range.maximum,
            })
            .collect(),
        response: match value.response {
            CompiledResponseMaterialV1::Json { mappings } => CompiledResponseShape::Json {
                mappings: mappings
                    .into_iter()
                    .map(|mapping| ResponseMapping {
                        pointer: mapping.pointer,
                        target: mapping.target,
                    })
                    .collect(),
            },
            CompiledResponseMaterialV1::RawBytes { target } => {
                CompiledResponseShape::RawBytes { target }
            }
        },
        selected_response_headers: value
            .selected_response_headers
            .into_iter()
            .map(selected_header_from_material)
            .collect::<Result<_, _>>()?,
        bounds: StepBounds {
            maximum_headers: nonzero_u32(value.bounds.maximum_headers, "maximum step headers")?,
            maximum_header_bytes: nonzero_u32(
                value.bounds.maximum_header_bytes,
                "maximum step header bytes",
            )?,
            maximum_url_bytes: nonzero_u32(
                value.bounds.maximum_url_bytes,
                "maximum step URL bytes",
            )?,
            maximum_request_bytes: nonzero_u32(
                value.bounds.maximum_request_bytes,
                "maximum step request bytes",
            )?,
            maximum_response_bytes: nonzero_u32(
                value.bounds.maximum_response_bytes,
                "maximum step response bytes",
            )?,
            maximum_json_depth: nonzero_u32(value.bounds.maximum_json_depth, "maximum JSON depth")?,
            maximum_json_nodes: nonzero_u32(value.bounds.maximum_json_nodes, "maximum JSON nodes")?,
            maximum_inline_binary_bytes: nonzero_u32(
                value.bounds.maximum_inline_binary_bytes,
                "maximum inline binary bytes",
            )?,
            deadline_ms: nonzero_u64_text(&value.bounds.deadline_ms, "step deadline")?,
        },
    })
}

fn binding_from_material(value: BindingMaterialV1) -> Result<CompiledBinding, CatalogError> {
    Ok(CompiledBinding {
        field: value.field,
        source: match value.source {
            CompiledBindingSourceMaterialV1::Input(()) => CompiledBindingSource::Input,
            CompiledBindingSourceMaterialV1::Constant { value } => {
                CompiledBindingSource::Constant {
                    value: typed_value_from_material(value)?,
                }
            }
        },
        required: value.required,
        default: value.default.map(typed_value_from_material).transpose()?,
        mapping: value.mapping,
    })
}

fn selected_header_from_material(
    value: SelectedResponseHeaderMaterialV1,
) -> Result<SelectedResponseHeader, CatalogError> {
    Ok(SelectedResponseHeader {
        canonical_lowercase_header_name: value.canonical_lowercase_header_name,
        capability: checked_manifest_primitive(
            CapabilityId::parse(&value.capability),
            "selected-header capability",
        )?,
    })
}

fn operation_bounds_from_material(
    value: OperationBoundsMaterialV1,
) -> Result<OperationBounds, CatalogError> {
    Ok(OperationBounds {
        maximum_calls: nonzero_u32(value.maximum_calls, "maximum operation calls")?,
        maximum_pages: nonzero_u32(value.maximum_pages, "maximum operation pages")?,
        maximum_items: nonzero_u32(value.maximum_items, "maximum operation items")?,
        maximum_aggregate_request_bytes: nonzero_u32(
            value.maximum_aggregate_request_bytes,
            "maximum aggregate request bytes",
        )?,
        maximum_aggregate_response_bytes: nonzero_u32(
            value.maximum_aggregate_response_bytes,
            "maximum aggregate response bytes",
        )?,
        maximum_output_canonical_bytes: nonzero_u32(
            value.maximum_output_canonical_bytes,
            "maximum canonical output bytes",
        )?,
        maximum_redirects: value.maximum_redirects,
        deadline_ms: nonzero_u64_text(&value.deadline_ms, "operation deadline")?,
    })
}

fn effect_from_material(value: OperationEffectMaterialV1) -> Result<OperationEffect, CatalogError> {
    Ok(match value {
        OperationEffectMaterialV1::ReadOnly(()) => OperationEffect::ReadOnly,
        OperationEffectMaterialV1::ProviderIdempotent { side_effect_steps } => {
            OperationEffect::ProviderIdempotent {
                side_effect_steps: side_effect_steps
                    .into_iter()
                    .map(|step| {
                        Ok(ProviderIdempotentStep {
                            step: checked_manifest_primitive(
                                CompiledStepId::parse(&step.step),
                                "idempotent step identity",
                            )?,
                            fixed_binding: match step.fixed_binding {
                                FixedIdempotencyBindingMaterialV1::Header { name } => {
                                    FixedIdempotencyBinding::Header { name }
                                }
                                FixedIdempotencyBindingMaterialV1::BodyField { pointer } => {
                                    FixedIdempotencyBinding::BodyField { pointer }
                                }
                            },
                            scope: step.scope,
                            minimum_retention_ms: nonzero_u64_text(
                                &step.minimum_retention_ms,
                                "idempotency retention",
                            )?,
                            clock_safety_margin_ms: nonzero_u64_text(
                                &step.clock_safety_margin_ms,
                                "idempotency clock margin",
                            )?,
                        })
                    })
                    .collect::<Result<_, CatalogError>>()?,
            }
        }
    })
}

fn pagination_bounds_from_material(
    value: PaginationBoundsMaterialV1,
) -> Result<PaginationBounds, CatalogError> {
    Ok(PaginationBounds {
        maximum_calls: nonzero_u32(value.maximum_calls, "maximum pagination calls")?,
        maximum_pages: nonzero_u32(value.maximum_pages, "maximum pagination pages")?,
        maximum_items: nonzero_u32(value.maximum_items, "maximum pagination items")?,
        maximum_response_bytes: nonzero_u32(
            value.maximum_response_bytes,
            "maximum page response bytes",
        )?,
        maximum_aggregate_response_bytes: nonzero_u32(
            value.maximum_aggregate_response_bytes,
            "maximum paginated response bytes",
        )?,
        maximum_output_canonical_bytes: nonzero_u32(
            value.maximum_output_canonical_bytes,
            "maximum paginated output bytes",
        )?,
    })
}

fn pagination_from_material(value: PaginationMaterialV1) -> Result<PaginationPlan, CatalogError> {
    Ok(match value {
        PaginationMaterialV1::None(()) => PaginationPlan::None,
        PaginationMaterialV1::Cursor {
            request_binding,
            response_pointer,
            bounds,
        } => PaginationPlan::Cursor {
            request_binding,
            response_pointer,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::OffsetLimit {
            offset_binding,
            limit_binding,
            initial_offset,
            page_size,
            bounds,
        } => PaginationPlan::OffsetLimit {
            offset_binding,
            limit_binding,
            initial_offset: u64_text(&initial_offset, "initial pagination offset")?,
            page_size: nonzero_u32(page_size, "pagination page size")?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::PageNumber {
            page_binding,
            page_size_binding,
            initial_page,
            page_size,
            bounds,
        } => PaginationPlan::PageNumber {
            page_binding,
            page_size_binding,
            initial_page: nonzero_u64_text(&initial_page, "initial page number")?,
            page_size: nonzero_u32(page_size, "pagination page size")?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::LinkRelation {
            relation,
            selected_header,
            bounds,
        } => PaginationPlan::LinkRelation {
            relation,
            selected_header: selected_header_from_material(selected_header)?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
        PaginationMaterialV1::Processor { processor, bounds } => PaginationPlan::Processor {
            processor: processor_ref_from_material(processor)?,
            bounds: pagination_bounds_from_material(bounds)?,
        },
    })
}

fn error_map_from_material(value: ErrorMapMaterialV1) -> Result<ErrorMap, CatalogError> {
    Ok(ErrorMap {
        rules: value
            .rules
            .into_iter()
            .map(|rule| {
                Ok(ErrorRule {
                    matcher: error_matcher_from_material(rule.matcher),
                    action: error_action_from_material(rule.action)?,
                })
            })
            .collect::<Result<_, CatalogError>>()?,
        fallback: CompleteErrorFallback {
            transport: error_action_from_material(value.fallback.transport)?,
            timeout: error_action_from_material(value.fallback.timeout)?,
            http_429: error_action_from_material(value.fallback.http_429)?,
            http_5xx: error_action_from_material(value.fallback.http_5xx)?,
            authentication: error_action_from_material(value.fallback.authentication)?,
            validation: error_action_from_material(value.fallback.validation)?,
            permanent: error_action_from_material(value.fallback.permanent)?,
            invariant: error_action_from_material(value.fallback.invariant)?,
        },
    })
}

fn error_matcher_from_material(value: ErrorMatcherMaterialV1) -> ErrorMatcher {
    match value {
        ErrorMatcherMaterialV1::Status(value) => ErrorMatcher::Status(StatusRange {
            minimum: value.minimum,
            maximum: value.maximum,
        }),
        ErrorMatcherMaterialV1::ProviderCode { pointer, codes } => {
            ErrorMatcher::ProviderCode { pointer, codes }
        }
        ErrorMatcherMaterialV1::Header { name, values } => ErrorMatcher::Header { name, values },
        ErrorMatcherMaterialV1::MalformedDeclaredSuccess(()) => {
            ErrorMatcher::MalformedDeclaredSuccess
        }
    }
}

fn error_action_from_material(value: ErrorActionMaterialV1) -> Result<ErrorAction, CatalogError> {
    let retry_after = match value.retry_after {
        RetryAfterMaterialV1::Never(()) => RetryAfterPolicy::Never,
        RetryAfterMaterialV1::RetryAfterHeader {
            step,
            capability,
            maximum_seconds,
        } => RetryAfterPolicy::RetryAfterHeader {
            step: checked_manifest_primitive(
                CompiledStepId::parse(&step),
                "retry-after step identity",
            )?,
            capability: checked_manifest_primitive(
                CapabilityId::parse(&capability),
                "retry-after capability",
            )?,
            maximum_seconds: nonzero_u32(maximum_seconds, "maximum retry-after seconds")?,
        },
    };
    let correlations = value
        .correlations
        .into_iter()
        .map(|correlation| {
            Ok(ErrorCorrelationBinding {
                canonical_lowercase_header_name: correlation.canonical_lowercase_header_name,
                capability: checked_manifest_primitive(
                    CapabilityId::parse(&correlation.capability),
                    "error-correlation capability",
                )?,
                step: checked_manifest_primitive(
                    CompiledStepId::parse(&correlation.step),
                    "error-correlation step identity",
                )?,
            })
        })
        .collect::<Result<_, CatalogError>>()?;
    ErrorAction::try_new(
        match value.class {
            ConnectorErrorClassMaterialV1::Transport(()) => ConnectorErrorClass::Transport,
            ConnectorErrorClassMaterialV1::Timeout(()) => ConnectorErrorClass::Timeout,
            ConnectorErrorClassMaterialV1::Http429(()) => ConnectorErrorClass::Http429,
            ConnectorErrorClassMaterialV1::Http5xx(()) => ConnectorErrorClass::Http5xx,
            ConnectorErrorClassMaterialV1::Authentication(()) => {
                ConnectorErrorClass::Authentication
            }
            ConnectorErrorClassMaterialV1::Validation(()) => ConnectorErrorClass::Validation,
            ConnectorErrorClassMaterialV1::Permanent(()) => ConnectorErrorClass::Permanent,
            ConnectorErrorClassMaterialV1::Invariant(()) => ConnectorErrorClass::Invariant,
        },
        &value.code,
        &value.safe_message,
        retry_after,
        correlations,
    )
    .map_err(|_| invalid_manifest_primitive("static error action"))
}

fn trigger_from_material(value: SemanticTriggerMaterialV1) -> Result<TriggerSpec, CatalogError> {
    Ok(match value {
        SemanticTriggerMaterialV1::Webhook {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            authenticator,
            codec,
            normalizer,
            selected_headers,
            raw_body_max_bytes,
            timestamp_window_ms,
            event_id,
            event_type,
            output,
            redaction,
            subscription_operations,
        } => TriggerSpec::Webhook {
            connector: checked_manifest_primitive(
                ConnectorId::parse(&connector),
                "webhook connector identity",
            )?,
            connector_version,
            trigger: checked_manifest_primitive(
                TriggerId::parse(&trigger),
                "webhook trigger identity",
            )?,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            authenticator: authenticator_ref_from_material(authenticator)?,
            codec: codec_ref_from_material(codec)?,
            normalizer: normalizer_ref_from_material(normalizer)?,
            selected_headers,
            raw_body_max_bytes: nonzero_u32(raw_body_max_bytes, "webhook body bound")?,
            timestamp_window_ms: nonzero_u64_text(
                &timestamp_window_ms,
                "webhook timestamp window",
            )?,
            event_id: value_contract_from_material(event_id),
            event_type: value_contract_from_material(event_type),
            output: value_contract_from_material(output),
            redaction: redaction_from_material(redaction),
            subscription_operations: subscription_operations
                .map(|operations| {
                    Ok(SubscriptionOperationIds {
                        create: checked_manifest_primitive(
                            OperationId::parse(&operations.create),
                            "subscription create operation",
                        )?,
                        delete: checked_manifest_primitive(
                            OperationId::parse(&operations.delete),
                            "subscription delete operation",
                        )?,
                        check: operations
                            .check
                            .map(|operation| {
                                checked_manifest_primitive(
                                    OperationId::parse(&operation),
                                    "subscription check operation",
                                )
                            })
                            .transpose()?,
                    })
                })
                .transpose()?,
        },
        SemanticTriggerMaterialV1::Poll {
            connector,
            connector_version,
            trigger,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            checkpoint,
            processor,
            event_type,
            per_poll_event_limit,
            bounds,
        } => TriggerSpec::Poll {
            connector: checked_manifest_primitive(
                ConnectorId::parse(&connector),
                "poll connector identity",
            )?,
            connector_version,
            trigger: checked_manifest_primitive(
                TriggerId::parse(&trigger),
                "poll trigger identity",
            )?,
            trigger_version,
            event_version,
            runtime_abi_epoch,
            checkpoint: value_contract_from_material(checkpoint),
            processor: processor_ref_from_material(processor)?,
            event_type: value_contract_from_material(event_type),
            per_poll_event_limit: nonzero_u32(per_poll_event_limit, "per-poll event limit")?,
            bounds: operation_bounds_from_material(bounds)?,
        },
    })
}

impl ResolvedFactValueMaterialV1 {
    pub fn use_site(&self) -> &str {
        &self.use_site
    }
}

impl ResolvedFactOriginMaterialV1 {
    pub fn use_site(&self) -> &str {
        &self.use_site
    }

    pub fn provider_evidence(&self) -> Option<(&str, &str, &str, ExactFactLocation)> {
        match &self.origin {
            ResolvedFactOriginV1::ProviderEvidence {
                source_record_id,
                fact_id,
                artifact_content_sha256,
                location,
            } => Some((
                source_record_id,
                fact_id,
                artifact_content_sha256,
                match location {
                    ExactFactLocationMaterialV1::JsonPointer { path, pointer } => {
                        ExactFactLocation::JsonPointer {
                            path: crate::SourcePath::parse(path)
                                .expect("material source paths are checked"),
                            pointer: pointer.clone(),
                        }
                    }
                    ExactFactLocationMaterialV1::DocumentSection { path, section } => {
                        ExactFactLocation::DocumentSection {
                            path: crate::SourcePath::parse(path)
                                .expect("material source paths are checked"),
                            section: section.clone(),
                        }
                    }
                },
            )),
            ResolvedFactOriginV1::DonatPolicy { policy_id: _ } => None,
        }
    }
}

fn resolve_fact_binding_contexts(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    requirements: &crate::CheckedFactRequirements,
    catalog: &AcceptedRecordCatalog,
    reviewed_policies: &BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ValidatedResolvedFactOriginContext>,
    ),
    CatalogError,
> {
    let mut semantic_by_use_site = BTreeMap::new();
    for binding in values {
        if binding.use_site.is_empty()
            || semantic_by_use_site
                .insert(binding.use_site.as_str(), binding)
                .is_some()
        {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "semantic fact use sites must be nonempty and unique",
            ));
        }
    }
    let mut origin_by_use_site = BTreeMap::new();
    for binding in origins {
        if binding.use_site.is_empty()
            || origin_by_use_site
                .insert(binding.use_site.as_str(), &binding.fact)
                .is_some()
        {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "provenance fact use sites must be nonempty and unique",
            ));
        }
    }
    if semantic_by_use_site.keys().ne(origin_by_use_site.keys()) {
        return Err(CatalogError::new(
            "catalog_fact_binding_mismatch",
            "semantic and provenance fact use-site sets differ",
        ));
    }

    let mut semantic = Vec::with_capacity(values.len());
    let mut provenance = Vec::with_capacity(origins.len());
    for (use_site, binding) in semantic_by_use_site {
        let value = &binding.value;
        let fact = origin_by_use_site
            .get(use_site)
            .expect("equal key sets were checked");
        let required_domain = requirements.required_domain(use_site).ok_or_else(|| {
            CatalogError::new(
                "catalog_fact_binding_mismatch",
                "fact use site has no checked normalized origin requirement",
            )
        })?;
        if !required_domain.accepts(fact) {
            return Err(CatalogError::new(
                "catalog_fact_binding_mismatch",
                "fact origin domain differs from the normalized use-site requirement",
            ));
        }
        let expected = typed_value_material(value);
        let origin = match fact {
            ContractFact::ProviderEvidence {
                source_record_id,
                fact_id,
            } => {
                let accepted = catalog.evidence_accepted(*source_record_id).map_err(|_| {
                    CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider source record is not accepted evidence",
                    )
                })?;
                let record = accepted.record();
                let contract_membership = record
                    .provider_contracts
                    .iter()
                    .filter(|contract| accepted.contracts().contains(&contract.contract_id))
                    .flat_map(|contract| &contract.facts)
                    .filter(|candidate| {
                        matches!(
                            candidate,
                            ContractFact::ProviderEvidence {
                                source_record_id: candidate_record,
                                fact_id: candidate_fact,
                            } if candidate_record == source_record_id && candidate_fact == fact_id
                        )
                    })
                    .count();
                if contract_membership != 1 {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider fact is not in the exact admitted contract closure",
                    ));
                }
                let SourceSubject::ProviderArtifact(provider) = &record.subject else {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "accepted evidence record has the wrong subject",
                    ));
                };
                let matches = provider
                    .evidence
                    .iter()
                    .flat_map(|artifact| {
                        artifact
                            .facts
                            .iter()
                            .filter(move |candidate| candidate.fact_id == *fact_id)
                            .map(move |candidate| (artifact, candidate))
                    })
                    .collect::<Vec<_>>();
                let [(artifact, provider_fact)] = matches.as_slice() else {
                    return Err(CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "provider fact does not resolve exactly once",
                    ));
                };
                if provider_fact.normalized_value != expected {
                    return Err(CatalogError::new(
                        "catalog_fact_binding_mismatch",
                        "semantic value differs from accepted provider evidence",
                    ));
                }
                ValidatedResolvedFactOrigin::ProviderEvidence {
                    source_record_id: *source_record_id,
                    fact_id: *fact_id,
                    artifact_content_sha256: artifact.content_sha256.clone(),
                    location: provider_fact.location.clone(),
                }
            }
            ContractFact::DonatPolicy {
                policy_id,
                value: declared,
            } => {
                let registered = reviewed_policies.get(policy_id).ok_or_else(|| {
                    CatalogError::new(
                        "catalog_fact_origin_unresolved",
                        "Donat policy is not in the reviewed registry",
                    )
                })?;
                let registered = typed_value_material(registered);
                if registered != *declared || registered != expected {
                    return Err(CatalogError::new(
                        "catalog_fact_binding_mismatch",
                        "semantic, declared, and reviewed policy values differ",
                    ));
                }
                ValidatedResolvedFactOrigin::DonatPolicy {
                    policy_id: *policy_id,
                }
            }
        };
        semantic.push(project_resolved_fact_value_material(binding)?);
        provenance.push(ValidatedResolvedFactOriginContext {
            use_site: use_site.to_owned(),
            origin,
        });
    }
    Ok((semantic, provenance))
}

pub fn resolve_fact_bindings(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    requirements: &crate::CheckedFactRequirements,
    catalog: &AcceptedRecordCatalog,
    reviewed_policies: &BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ResolvedFactOriginMaterialV1>,
    ),
    CatalogError,
> {
    let (semantic, provenance) =
        resolve_fact_binding_contexts(values, origins, requirements, catalog, reviewed_policies)?;
    let provenance = provenance
        .iter()
        .map(resolved_fact_origin_material)
        .collect::<Result<Vec<_>, CatalogError>>()?;
    Ok((semantic, provenance))
}

fn validate_source_projection_input(record: &ConnectorSourceRecord) -> Result<(), CatalogError> {
    let encoded = crate::canonical_yaml(record)?;
    let checked = crate::load_record_bytes(&encoded)?;
    if checked != *record {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "source record changed during checked reconstruction",
        ));
    }
    Ok(())
}

pub fn decode_source_record_material(bytes: &[u8]) -> Result<SourceRecordMaterialV1, CatalogError> {
    let record = crate::load_record_bytes(bytes)
        .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
    source_record_material(&record)
}

fn sorted_unique_strings<T: AsRef<str>>(values: &[T]) -> Result<Vec<String>, CatalogError> {
    let mut values = values
        .iter()
        .map(|value| value.as_ref().to_owned())
        .collect::<Vec<_>>();
    values.sort();
    reject_adjacent_duplicate(values.iter().map(String::as_str))?;
    Ok(values)
}

fn artifact_hash_material_key(value: &ArtifactHashMaterialV1) -> &str {
    &value.artifact_id
}

fn dependency_decision_material_key(value: &DependencyDecisionMaterialV1) -> &str {
    &value.dependency
}

fn embedded_decision_material_key(value: &EmbeddedDecisionMaterialV1) -> &str {
    &value.material_id
}

fn provider_contract_material_key(value: &ProviderContractMaterialV1) -> &str {
    &value.contract_id
}

fn verified_npm_signature_material_key(value: &VerifiedNpmSignatureMaterialV1) -> &str {
    &value.key_id
}

fn provider_evidence_material_key(value: &ProviderEvidenceMaterialV1) -> String {
    format!(
        "{}\0{}",
        provider_evidence_source_key(&value.source),
        value.content_sha256
    )
}

fn provider_evidence_source_key(source: &ProviderEvidenceSourceMaterialV1) -> String {
    match source {
        ProviderEvidenceSourceMaterialV1::RepositoryFile {
            repository,
            commit,
            path,
        } => format!("repository:{repository}\0{commit}\0{path}"),
        ProviderEvidenceSourceMaterialV1::VersionedArtifact {
            url,
            provider_revision,
        } => format!("artifact:{url}\0{provider_revision}"),
    }
}

fn provider_fact_material_key(value: &ProviderFactMaterialV1) -> &str {
    &value.fact_id
}

fn repo_file_hash_material_key(value: &RepoFileHashMaterialV1) -> &str {
    &value.path
}

fn contract_fact_material_key(value: &ContractFactMaterialV1) -> String {
    match value {
        ContractFactMaterialV1::ProviderEvidence {
            source_record_id,
            fact_id,
        } => format!("provider:{source_record_id}:{fact_id}"),
        ContractFactMaterialV1::DonatPolicy {
            policy_id,
            value: _,
        } => format!("policy:{policy_id}"),
    }
}

fn safety_finding_material_key(value: &SafetyFindingMaterialV1) -> &str {
    &value.finding_id
}

fn reject_adjacent_duplicate<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<(), CatalogError> {
    let mut previous: Option<&str> = None;
    for value in values {
        if previous == Some(value) {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "duplicate set-like material key",
            ));
        }
        previous = Some(value);
    }
    Ok(())
}

fn validate_semantic_canonical_schema_epoch(value: u32) -> Result<u32, CatalogError> {
    if value == 0 {
        Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "canonical schema epoch must be nonzero",
        ))
    } else {
        Ok(value)
    }
}

fn validate_semantic_value_language_epoch(value: u32) -> Result<u32, CatalogError> {
    if value == 0 {
        Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-language epoch must be nonzero",
        ))
    } else {
        Ok(value)
    }
}

trait MaterialId {
    fn material_id(&self) -> &str;
}

macro_rules! material_id_impl {
    ($($type:ty),+ $(,)?) => {
        $(impl MaterialId for $type {
            fn material_id(&self) -> &str {
                self.as_str()
            }
        })+
    };
}

material_id_impl!(
    donat_connector_abi::AuthenticatorId,
    donat_connector_abi::CodecId,
    donat_connector_abi::NormalizerId,
    donat_connector_abi::ProcessorFamilyId,
);

fn semantic_trigger_key(value: &SemanticTriggerMaterialV1) -> (&'static str, &str) {
    match value {
        SemanticTriggerMaterialV1::Poll {
            connector: _,
            connector_version: _,
            trigger,
            trigger_version: _,
            event_version: _,
            runtime_abi_epoch: _,
            checkpoint: _,
            processor: _,
            event_type: _,
            per_poll_event_limit: _,
            bounds: _,
        } => ("poll", trigger),
        SemanticTriggerMaterialV1::Webhook {
            connector: _,
            connector_version: _,
            trigger,
            trigger_version: _,
            event_version: _,
            runtime_abi_epoch: _,
            authenticator: _,
            codec: _,
            normalizer: _,
            selected_headers: _,
            raw_body_max_bytes: _,
            timestamp_window_ms: _,
            event_id: _,
            event_type: _,
            output: _,
            redaction: _,
            subscription_operations: _,
        } => ("webhook", trigger),
    }
}

fn sort_and_deduplicate_materials<T>(values: &mut Vec<T>) -> Result<(), CatalogError>
where
    T: Clone + Eq + Serialize,
{
    let mut keyed = values
        .drain(..)
        .map(|value| canonical_material_bytes(&value).map(|key| (key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed.dedup_by(|left, right| left.0 == right.0);
    values.extend(keyed.into_iter().map(|(_, value)| value));
    Ok(())
}

fn projection_input_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_projection_input_mismatch",
        detail,
    ))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_material_name(value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.chars().any(|character| {
            let scalar = u32::from(character);
            (0xfdd0..=0xfdef).contains(&scalar)
                || scalar & 0xffff == 0xfffe
                || scalar & 0xffff == 0xffff
        })
    {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-contract name is empty or not I-JSON",
        ));
    }
    Ok(())
}

pub fn decode_value_contract_material(
    bytes: &[u8],
) -> Result<ValueContractMaterialV1, CatalogError> {
    let canonical = canonicalize_raw(bytes)?;
    let decoded: ValueContractMaterialDto =
        serde_json::from_slice(&canonical).map_err(|error| {
            CatalogError::new("catalog_projection_input_mismatch", error.to_string())
        })?;
    value_contract_material_from_dto(decoded)
}

fn value_contract_material_from_dto(
    decoded: ValueContractMaterialDto,
) -> Result<ValueContractMaterialV1, CatalogError> {
    if decoded.value_language_epoch == 0 {
        return Err(CatalogError::new(
            "catalog_projection_input_mismatch",
            "value-language epoch must be nonzero",
        ));
    }
    validate_decoded_value_contract(&decoded)?;
    Ok(ValueContractMaterialV1 {
        named_objects: decoded.named_objects,
        roots: decoded.roots,
        value_language_epoch: decoded.value_language_epoch,
    })
}

fn validate_decoded_value_contract(value: &ValueContractMaterialDto) -> Result<(), CatalogError> {
    let mut type_refs = Vec::new();
    for (name, field) in &value.roots {
        validate_material_name(name)?;
        type_refs.push(&field.type_ref);
    }
    for (name, object) in &value.named_objects {
        validate_material_name(name)?;
        for (field_name, field) in &object.fields {
            validate_material_name(field_name)?;
            type_refs.push(&field.type_ref);
        }
    }
    while let Some(type_ref) = type_refs.pop() {
        let TypeRefMaterialV1 {
            nullable: _,
            value_type,
        } = type_ref;
        match &value_type.0 {
            ValueTypeMaterial::Scalar(ValueScalarMaterialV1(ValueScalarMaterial::Custom {
                name,
            }))
            | ValueTypeMaterial::Ref { name } => validate_material_name(name)?,
            ValueTypeMaterial::Enum { name, values } => {
                validate_material_name(name)?;
                for value in values {
                    validate_material_name(value)?;
                }
            }
            ValueTypeMaterial::Object {
                fields: nested_fields,
            } => {
                for (name, field) in nested_fields {
                    validate_material_name(name)?;
                    type_refs.push(&field.type_ref);
                }
            }
            ValueTypeMaterial::List { element } => type_refs.push(element.as_ref()),
            ValueTypeMaterial::Scalar(_) => {}
        }
    }
    Ok(())
}

pub fn canonical_material_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CatalogError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
    canonicalize_raw(&bytes)
}

fn hash_material<T: Serialize>(
    domain: CatalogHashDomain,
    value: &T,
) -> Result<AbiHash256, CatalogError> {
    Ok(AbiHash256::new(domain_hash_bytes(
        domain,
        &canonical_material_bytes(value)?,
    )))
}

pub fn record_sha256(value: &SourceRecordMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::SourceRecord, value)
}

pub fn semantic_sha256(value: &SemanticMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Semantic, value)
}

pub fn provenance_sha256(value: &ProvenanceMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Provenance, value)
}

pub fn value_contract_sha256(value: &ValueContractMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::ValueContract, value)
}

pub fn canonical_projection_owner_manifest() -> &'static str {
    const DOCUMENT: &str = include_str!(
        "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
    );
    const OPEN: &str = "```text\nnormalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type\n";
    let start = DOCUMENT
        .find(OPEN)
        .expect("accepted ADR 012 must contain the owner manifest")
        + "```text\n".len();
    let end = DOCUMENT[start..]
        .find("\n```")
        .expect("owner manifest code block must terminate")
        + start;
    &DOCUMENT[start..end]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerManifestValidation {
    pub mapping_rows: usize,
    pub normalized_leaf_and_branch_total: usize,
}

pub fn validate_canonical_owner_manifest() -> Result<OwnerManifestValidation, CatalogError> {
    validate_canonical_owner_manifest_text(canonical_projection_owner_manifest())
}

fn validate_canonical_owner_manifest_text(
    manifest: &str,
) -> Result<OwnerManifestValidation, CatalogError> {
    let mut lines = manifest.lines();
    let header = lines.next().ok_or_else(|| {
        CatalogError::new("catalog_projection_manifest_invalid", "missing header")
    })?;
    if header != "normalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type" {
        return Err(CatalogError::new(
            "catalog_projection_manifest_invalid",
            "unexpected owner-manifest header",
        ));
    }
    let mut owner_domains = BTreeSet::new();
    let mut canonical_paths = BTreeSet::new();
    let mut mapping_rows = 0;
    for line in lines {
        let columns: Vec<_> = line.split('|').collect();
        if columns.len() != 7
            || columns.iter().any(|column| column.is_empty())
            || columns.iter().any(|column| {
                *column == "*"
                    || *column == "family"
                    || column.contains("<family>")
                    || column.contains("all fields")
                    || column.contains("corresponding")
            })
            || !matches!(
                columns[1],
                "source-record" | "semantic" | "provenance" | "value-contract"
            )
            || (!columns[3].eq("normalized")
                && !columns[3].eq("constant")
                && !columns[3].starts_with("derived:"))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_invalid",
                line,
            ));
        }
        if !owner_domains.insert((columns[0], columns[1]))
            || !canonical_paths.insert((columns[1], columns[2]))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_invalid",
                line,
            ));
        }
        mapping_rows += 1;
    }
    if mapping_rows == 0 || owner_domains.is_empty() {
        return Err(CatalogError::new(
            "catalog_projection_manifest_incomplete",
            "owner manifest contains no mappings",
        ));
    }
    Ok(OwnerManifestValidation {
        mapping_rows,
        normalized_leaf_and_branch_total: owner_domains.len(),
    })
}

pub fn selected_response_header(
    connector: ConnectorId,
    operation: OperationId,
    operation_version: StableSemver,
    step: CompiledStepId,
    header: &str,
) -> Result<SelectedResponseHeader, CatalogError> {
    let canonical_header = canonical_header_name(header)?;
    let bytes = format!(
        "{{\"connector\":{},\"header\":{},\"operation\":{},\"operation_version\":{{\"major\":{},\"minor\":{},\"patch\":{}}},\"step\":{}}}",
        json_string(connector.as_str()),
        json_string(&canonical_header),
        json_string(operation.as_str()),
        operation_version.major,
        operation_version.minor,
        operation_version.patch,
        json_string(step.as_str()),
    );
    let mut hash = Sha256::new();
    hash.update(RESPONSE_HEADER_DOMAIN);
    hash.update(bytes.as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut value = String::from("response-header.");
    for byte in digest {
        use core::fmt::Write;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    let capability = CapabilityId::parse(&value)
        .map_err(|_| CatalogError::new("catalog_selected_header_invalid", "capability ID"))?;
    Ok(SelectedResponseHeader {
        canonical_lowercase_header_name: canonical_header,
        capability,
    })
}

fn canonical_header_name(header: &str) -> Result<String, CatalogError> {
    if header.is_empty()
        || !header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(CatalogError::new(
            "catalog_selected_header_invalid",
            "header name is not an ASCII token",
        ));
    }
    Ok(header.to_ascii_lowercase())
}

pub fn canonicalize_raw(bytes: &[u8]) -> Result<Vec<u8>, CatalogError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| CatalogError::new("catalog_jcs_invalid_utf8", "input is not valid UTF-8"))?;
    validate_json_string_unicode(source)?;
    let number_tokens = scan_number_tokens(source)?;
    let number_cursor = Cell::new(0);

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let mut value = JValueSeed {
        source,
        number_tokens: &number_tokens,
        number_cursor: &number_cursor,
    }
    .deserialize(&mut deserializer)
    .map_err(map_json_error)?;
    deserializer.end().map_err(map_json_error)?;
    if number_cursor.get() != number_tokens.len() {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "raw number token cursor did not consume the complete input",
        ));
    }
    value.canonicalize_numbers()?;
    let mut output = Vec::new();
    value.write_canonical(&mut output);
    Ok(output)
}

fn scan_number_tokens(source: &str) -> Result<Vec<Range<usize>>, CatalogError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut in_string = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if !in_string => {
                in_string = true;
                index += 1;
            }
            b'"' if in_string => {
                in_string = false;
                index += 1;
            }
            b'\\' if in_string => {
                index = index.saturating_add(2);
            }
            b'-' | b'0'..=b'9' if !in_string => {
                let end = scan_json_number(bytes, index)?;
                tokens.push(index..end);
                index = end;
            }
            _ => index += 1,
        }
    }
    Ok(tokens)
}

fn scan_json_number(bytes: &[u8], start: usize) -> Result<usize, CatalogError> {
    let mut index = start;
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    }
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number integer component",
            ));
        }
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number fraction",
            ));
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "invalid JSON number exponent",
            ));
        }
    }
    Ok(index)
}

fn canonical_number(raw: &str) -> Result<String, CatalogError> {
    let number = raw.parse::<f64>().map_err(|_| {
        CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number is not finite binary64",
        )
    })?;
    if !number.is_finite() {
        return Err(CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number is not finite binary64",
        ));
    }
    let mut buffer = ryu_js::Buffer::new();
    let canonical = buffer.format_finite(number);
    if decimal_identity(raw)? != decimal_identity(canonical)? {
        return Err(CatalogError::new(
            "canonical_json_number_not_exact",
            "raw number changes mathematical value as binary64",
        ));
    }
    Ok(canonical.to_owned())
}

#[derive(Debug, Eq, PartialEq)]
enum DecimalIdentity {
    Zero,
    NonZero {
        negative: bool,
        coefficient: String,
        exponent: i64,
    },
}

fn decimal_identity(raw: &str) -> Result<DecimalIdentity, CatalogError> {
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, "0"), |parts| parts);
    let (integer, fraction) = mantissa
        .split_once('.')
        .map_or((mantissa, ""), |parts| parts);
    let mut coefficient = String::with_capacity(integer.len() + fraction.len());
    coefficient.push_str(integer);
    coefficient.push_str(fraction);
    let coefficient = coefficient.trim_start_matches('0');
    if coefficient.is_empty() {
        return Ok(DecimalIdentity::Zero);
    }
    let trailing_zeros = coefficient.len() - coefficient.trim_end_matches('0').len();
    let coefficient = coefficient[..coefficient.len() - trailing_zeros].to_owned();
    let exponent = parse_decimal_exponent(exponent)?
        .checked_sub(i64::try_from(fraction.len()).map_err(|_| number_not_exact())?)
        .and_then(|value| value.checked_add(i64::try_from(trailing_zeros).ok()?))
        .ok_or_else(number_not_exact)?;
    Ok(DecimalIdentity::NonZero {
        negative,
        coefficient,
        exponent,
    })
}

fn parse_decimal_exponent(value: &str) -> Result<i64, CatalogError> {
    let (negative, digits) = value.strip_prefix('-').map_or_else(
        || (false, value.strip_prefix('+').unwrap_or(value)),
        |digits| (true, digits),
    );
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let magnitude = digits.parse::<i64>().map_err(|_| number_not_exact())?;
    Ok(if negative { -magnitude } else { magnitude })
}

fn number_not_exact() -> CatalogError {
    CatalogError::new(
        "canonical_json_number_not_exact",
        "raw number exponent is outside the exact comparison range",
    )
}

fn map_json_error(error: serde_json::Error) -> CatalogError {
    let message = error.to_string();
    if message.contains("duplicate decoded member") {
        CatalogError::new("catalog_jcs_duplicate_member", message)
    } else if message.contains("canonical_json_number_not_exact")
        || message.contains("number out of range")
        || message.contains("number is not exactly representable")
    {
        CatalogError::new("canonical_json_number_not_exact", message)
    } else {
        CatalogError::new("catalog_jcs_schema_mismatch", message)
    }
}

fn validate_json_string_unicode(source: &str) -> Result<(), CatalogError> {
    let bytes = source.as_bytes();
    let mut byte_index = 0;
    let mut in_string = false;
    while byte_index < bytes.len() {
        let byte = bytes[byte_index];
        if !in_string {
            if byte == b'"' {
                in_string = true;
            }
            byte_index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = false;
                byte_index += 1;
            }
            b'\\' => {
                byte_index += 1;
                if byte_index >= bytes.len() {
                    break;
                }
                if bytes[byte_index] != b'u' {
                    byte_index += 1;
                    continue;
                }
                let (first, next) = read_hex_escape(bytes, byte_index)?;
                byte_index = next;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if byte_index + 1 >= bytes.len()
                        || bytes[byte_index] != b'\\'
                        || bytes[byte_index + 1] != b'u'
                    {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "lone high surrogate",
                        ));
                    }
                    let (second, following) = read_hex_escape(bytes, byte_index + 1)?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "high surrogate is not followed by a low surrogate",
                        ));
                    }
                    byte_index = following;
                    0x1_0000 + (((u32::from(first) - 0xd800) << 10) | (u32::from(second) - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(CatalogError::new(
                        "catalog_jcs_invalid_surrogate",
                        "lone low surrogate",
                    ));
                } else {
                    u32::from(first)
                };
                reject_noncharacter(scalar)?;
            }
            _ if byte.is_ascii() => byte_index += 1,
            _ => {
                let character = source[byte_index..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 has a character");
                reject_noncharacter(u32::from(character))?;
                byte_index += character.len_utf8();
            }
        }
    }
    Ok(())
}

fn read_hex_escape(bytes: &[u8], u_index: usize) -> Result<(u16, usize), CatalogError> {
    if u_index + 5 > bytes.len() || bytes[u_index] != b'u' {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "incomplete Unicode escape",
        ));
    }
    let mut value = 0_u16;
    for byte in &bytes[u_index + 1..u_index + 5] {
        value = value
            .checked_mul(16)
            .and_then(|current| {
                let digit = match byte {
                    b'0'..=b'9' => u16::from(*byte - b'0'),
                    b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                    _ => return None,
                };
                current.checked_add(digit)
            })
            .ok_or_else(|| {
                CatalogError::new("catalog_jcs_schema_mismatch", "invalid Unicode escape")
            })?;
    }
    Ok((value, u_index + 5))
}

fn reject_noncharacter(scalar: u32) -> Result<(), CatalogError> {
    if (0xfdd0..=0xfdef).contains(&scalar) || scalar & 0xffff == 0xfffe || scalar & 0xffff == 0xffff
    {
        Err(CatalogError::new(
            "catalog_jcs_disallowed_unicode",
            "Unicode noncharacters are not I-JSON",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
enum JValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl JValue {
    fn canonicalize_numbers(&mut self) -> Result<(), CatalogError> {
        match self {
            Self::Number(value) => *value = canonical_number(value)?,
            Self::Array(values) => {
                for value in values {
                    value.canonicalize_numbers()?;
                }
            }
            Self::Object(values) => {
                for (_, value) in values {
                    value.canonicalize_numbers()?;
                }
            }
            Self::Null | Self::Bool(_) | Self::String(_) => {}
        }
        Ok(())
    }

    fn write_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Null => output.extend_from_slice(b"null"),
            Self::Bool(true) => output.extend_from_slice(b"true"),
            Self::Bool(false) => output.extend_from_slice(b"false"),
            Self::Number(value) => output.extend_from_slice(value.as_bytes()),
            Self::String(value) => output.extend_from_slice(json_string(value).as_bytes()),
            Self::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    value.write_canonical(output);
                }
                output.push(b']');
            }
            Self::Object(values) => {
                let mut values: Vec<_> = values.iter().collect();
                values.sort_by(|left, right| utf16_cmp(&left.0, &right.0));
                output.push(b'{');
                for (index, (name, value)) in values.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    output.extend_from_slice(json_string(name).as_bytes());
                    output.push(b':');
                    value.write_canonical(output);
                }
                output.push(b'}');
            }
        }
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("valid Rust strings are JSON strings")
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[derive(Clone, Copy)]
struct JValueSeed<'a> {
    source: &'a str,
    number_tokens: &'a [Range<usize>],
    number_cursor: &'a Cell<usize>,
}

impl<'de> DeserializeSeed<'de> for JValueSeed<'_> {
    type Value = JValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(JValueVisitor {
            source: self.source,
            number_tokens: self.number_tokens,
            number_cursor: self.number_cursor,
        })
    }
}

struct JValueVisitor<'a> {
    source: &'a str,
    number_tokens: &'a [Range<usize>],
    number_cursor: &'a Cell<usize>,
}

impl JValueVisitor<'_> {
    fn next_number<E>(&self) -> Result<String, E>
    where
        E: serde::de::Error,
    {
        let index = self.number_cursor.get();
        let range = self
            .number_tokens
            .get(index)
            .ok_or_else(|| E::custom("raw number token cursor exhausted"))?;
        self.number_cursor.set(index + 1);
        let raw = self
            .source
            .get(range.clone())
            .ok_or_else(|| E::custom("raw number token range is outside the input"))?;
        Ok(raw.to_owned())
    }
}

impl<'de> Visitor<'de> for JValueVisitor<'_> {
    type Value = JValue;

    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("one I-JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(JValue::Bool(value))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.next_number().map(JValue::Number)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(JValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(JValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JValueSeed {
            source: self.source,
            number_tokens: self.number_tokens,
            number_cursor: self.number_cursor,
        })? {
            values.push(value);
        }
        Ok(JValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        let mut values = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            if !names.insert(name.clone()) {
                return Err(serde::de::Error::custom("duplicate decoded member"));
            }
            values.push((
                name,
                map.next_value_seed(JValueSeed {
                    source: self.source,
                    number_tokens: self.number_tokens,
                    number_cursor: self.number_cursor,
                })?,
            ));
        }
        Ok(JValue::Object(values))
    }
}
