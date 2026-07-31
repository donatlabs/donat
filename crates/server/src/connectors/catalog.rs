use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use donat_connector_abi::{
    AuthenticatorId, CodecId, CompiledStepId, ConnectorId, NormalizerId, OperationId, OriginId,
    TriggerId,
};
use donat_connector_catalog::{
    CapacityDefaults, CompiledBinding, CompiledBindingSource, CompiledHeaderBinding,
    CompiledQueryBinding, CompiledRequestShape, CompiledResponseShape, CompiledStepSpec,
    CompleteErrorFallback, ErrorAction, ErrorMap, ErrorMatcher, ErrorRule, FixedIdempotencyBinding,
    FixedOrigin, HttpsOnly, NetworkPolicy, OperationBounds, OperationEffect, OperationSpec,
    PaginationPlan, ProviderIdempotentStep, RateDefaults, RedactionPlan, ResponseMapping,
    RetryAfterPolicy, StableSemver, StatusRange, StepBounds, TriggerSpec, VersionedProcessorRef,
    value_contract_material, value_contract_sha256,
};
use donat_ir::{
    TypeRef, TypedValue, VALUE_TYPE_LANGUAGE_VERSION, ValueContractCatalog, ValueContractField,
    ValueObjectContract, ValueScalar, ValueType,
};
use donat_metadata::{
    ConnectorEffect, ConnectorError, ConnectorErrorClass, ConnectorInstance, ConnectorOperation,
    CustomTypeField, EnumValue, Metadata, RuleTypeDeclaration,
};

use super::ConnectorDefinition;

const REQUEST_STEP: CompiledStepId = CompiledStepId::literal("request");

pub(super) fn compile_stripe_checkout_completed_trigger_spec(
    metadata: &Metadata,
    definition: ConnectorDefinition,
) -> Result<TriggerSpec, String> {
    let declarations = ContractDeclarations::new(metadata)?;
    let event_id = declarations.contract([("provider_event_id", "string!")].into_iter())?;
    let event_type = declarations.contract([("event_type", "string!")].into_iter())?;
    let output = declarations.contract(
        [
            ("provider_event_id", "string!"),
            ("event_type", "string!"),
            ("checkout_session_id", "string!"),
            ("client_reference_id", "uuid!"),
            ("payment_status", "string!"),
        ]
        .into_iter(),
    )?;
    Ok(TriggerSpec::Webhook {
        connector: ConnectorId::parse(definition.module_name)
            .map_err(|_| "Stripe connector name is not a canonical ABI ID".to_owned())?,
        connector_version: parse_stable_semver(definition.semantic_version)?,
        trigger: TriggerId::parse(super::stripe::COMPLETED_WEBHOOK_TRIGGER)
            .map_err(|_| "Stripe webhook trigger is not a canonical ABI ID".to_owned())?,
        trigger_version: parse_stable_semver(super::stripe::STRIPE_TRIGGER_VERSION)?,
        event_version: StableSemver::new(1, 0, 0),
        runtime_abi_epoch: definition.runtime_abi,
        authenticator: VersionedProcessorRef {
            id: AuthenticatorId::literal("stripe.webhook.signature"),
            implementation_revision: 1,
        },
        codec: VersionedProcessorRef {
            id: CodecId::literal("json"),
            implementation_revision: 1,
        },
        normalizer: VersionedProcessorRef {
            id: NormalizerId::literal("stripe.checkout.completed"),
            implementation_revision: 1,
        },
        selected_headers: vec!["stripe-signature".to_owned()],
        raw_body_max_bytes: NonZeroU32::new(super::http::MAX_HTTP_BODY_BYTES as u32)
            .expect("the compiled HTTP body limit is nonzero"),
        timestamp_window_ms: NonZeroU64::new(300_000)
            .expect("the Stripe timestamp window is nonzero"),
        event_id,
        event_type,
        output,
        redaction: RedactionPlan::Omit,
        subscription_operations: None,
    })
}

pub(super) fn compile_http_operation_spec(
    metadata: &Metadata,
    definition: ConnectorDefinition,
    instance: &ConnectorInstance,
    operation: &ConnectorOperation,
) -> Result<Option<OperationSpec>, String> {
    let http = operation
        .http()
        .ok_or_else(|| "HTTP catalog operation has no HTTP profile".to_owned())?;

    // Existing hand-written operations that predate the finite catalog
    // contract remain runnable through the legacy transport path, but they are
    // inventory-only and must not be visible to process compilation.
    let (Some(effect), Some(bounds), Some(error_map), Some(capacity)) = (
        http.effect.as_ref(),
        http.bounds.as_ref(),
        http.error_map.as_ref(),
        operation.capacity(),
    ) else {
        return Ok(None);
    };

    let connector = ConnectorId::parse(definition.module_name)
        .map_err(|_| "connector module name is not a canonical ABI ID".to_owned())?;
    let operation_id = OperationId::parse(&operation.name)
        .map_err(|_| "connector operation name is not a canonical ABI ID".to_owned())?;
    let connector_version = parse_stable_semver(definition.semantic_version)?;
    let operation_version = parse_stable_semver(&http.version)?;
    let origin = OriginId::parse(&format!("instance.{}", instance.name))
        .map_err(|_| "connector instance name cannot form a canonical origin ID".to_owned())?;

    let declarations = ContractDeclarations::new(metadata)?;
    let input = declarations.contract(
        http.input_contract
            .iter()
            .map(|(name, type_)| (name.as_str(), type_.as_str())),
    )?;
    let output = declarations.contract(
        http.response
            .iter()
            .map(|(name, binding)| (name.as_str(), binding.type_.as_str())),
    )?;
    let input_contract_sha256 = contract_hash(&input)?;
    let output_contract_sha256 = contract_hash(&output)?;

    let maximum_request_bytes = u64_to_nonzero_u32(
        bounds.maximum_aggregate_request_bytes,
        "maximum aggregate request bytes",
    )?;
    let maximum_response_bytes = u64_to_nonzero_u32(
        bounds.maximum_aggregate_response_bytes,
        "maximum aggregate response bytes",
    )?;
    let maximum_header_count = instance
        .config
        .headers
        .len()
        .checked_add(http.headers.len())
        .and_then(|count| {
            count.checked_add(usize::from(matches!(
                effect,
                ConnectorEffect::ProviderIdempotent { .. }
            )))
        })
        .ok_or_else(|| "connector header count overflowed".to_owned())?
        .max(1);

    let query = http
        .query
        .iter()
        .map(|(name, binding)| CompiledQueryBinding {
            name: name.to_ascii_lowercase(),
            binding: CompiledBinding {
                field: binding.input.clone(),
                source: CompiledBindingSource::Input,
                required: input
                    .roots
                    .get(&binding.input)
                    .is_some_and(|field| field.required),
                default: None,
                mapping: None,
            },
        })
        .collect();
    let mut headers = http
        .headers
        .iter()
        .enumerate()
        .map(|(index, header)| CompiledHeaderBinding {
            name: header.name.to_ascii_lowercase(),
            binding: CompiledBinding {
                field: format!("static_header_{index}"),
                source: CompiledBindingSource::Constant {
                    value: TypedValue::String(header.value.clone()),
                },
                required: true,
                default: None,
                mapping: None,
            },
        })
        .collect::<Vec<_>>();
    headers.sort_by(|left, right| left.name.cmp(&right.name));

    let request = match &http.body {
        None => CompiledRequestShape::None,
        Some(body) => CompiledRequestShape::Json {
            bindings: body_bindings(body),
        },
    };
    let response = CompiledResponseShape::Json {
        mappings: http
            .response
            .iter()
            .map(|(target, binding)| ResponseMapping {
                pointer: binding.json_pointer.clone(),
                target: target.clone(),
            })
            .collect(),
    };

    let spec = OperationSpec {
        connector,
        connector_version,
        operation: operation_id,
        operation_version,
        runtime_abi_epoch: definition.runtime_abi,
        value_language_epoch: VALUE_TYPE_LANGUAGE_VERSION,
        input,
        input_contract_sha256,
        output,
        output_contract_sha256,
        credential: None,
        origins: vec![FixedOrigin {
            origin,
            scheme: HttpsOnly,
            // This migration bridge retains only the non-secret deployment
            // endpoint identity. The resolved base URL remains private to the
            // existing transport instance.
            host: instance.config.endpoint_identity.clone(),
            port: NonZeroU16::new(443).expect("HTTPS port is nonzero"),
            network_policy: NetworkPolicy::PublicOnly,
        }],
        steps: vec![CompiledStepSpec {
            step: REQUEST_STEP,
            method: http.method.clone(),
            origin,
            path: http.path.clone(),
            query,
            headers,
            credential_action: None,
            request,
            success_statuses: http
                .success_statuses
                .iter()
                .copied()
                .map(|status| StatusRange {
                    minimum: status,
                    maximum: status,
                })
                .collect(),
            response,
            selected_response_headers: Vec::new(),
            bounds: StepBounds {
                maximum_headers: nonzero_u32(
                    u32::try_from(maximum_header_count)
                        .map_err(|_| "connector header count exceeds u32".to_owned())?,
                    "maximum headers",
                )?,
                maximum_header_bytes: maximum_request_bytes,
                maximum_url_bytes: maximum_request_bytes,
                maximum_request_bytes,
                maximum_response_bytes,
                maximum_json_depth: nonzero_u32(bounds.maximum_json_depth, "maximum JSON depth")?,
                maximum_json_nodes: nonzero_u32(bounds.maximum_json_nodes, "maximum JSON nodes")?,
                maximum_inline_binary_bytes: NonZeroU32::new(1)
                    .expect("one inline byte is nonzero"),
                deadline_ms: nonzero_u64(bounds.deadline_ms, "operation deadline")?,
            },
        }],
        pre_request_transforms: Vec::new(),
        post_response_transforms: Vec::new(),
        operation_processor: None,
        effect: compile_effect(effect)?,
        pagination: PaginationPlan::None,
        error_map: compile_error_map(error_map)?,
        capacity: CapacityDefaults {
            maximum_in_flight: nonzero_u32(capacity.max_in_flight, "maximum in-flight")?,
        },
        rate: RateDefaults {
            burst: nonzero_u32(capacity.rate_limit.burst, "rate burst")?,
            refill_interval_ms: rate_refill_interval_ms(
                capacity.rate_limit.permits,
                &capacity.rate_limit.per,
            )?,
        },
        serialization_key_default: None,
        bounds: OperationBounds {
            maximum_calls: nonzero_u32(bounds.maximum_calls, "maximum calls")?,
            maximum_pages: nonzero_u32(bounds.maximum_pages, "maximum pages")?,
            maximum_items: nonzero_u32(bounds.maximum_items, "maximum items")?,
            maximum_aggregate_request_bytes: maximum_request_bytes,
            maximum_aggregate_response_bytes: maximum_response_bytes,
            maximum_output_canonical_bytes: u64_to_nonzero_u32(
                bounds.maximum_output_canonical_bytes,
                "maximum canonical output bytes",
            )?,
            maximum_redirects: u8::try_from(bounds.maximum_redirects)
                .map_err(|_| "maximum redirects exceeds u8".to_owned())?,
            deadline_ms: nonzero_u64(bounds.deadline_ms, "operation deadline")?,
        },
        resolved_fact_values: Vec::new(),
    };
    spec.validate()
        .map_err(|error| format!("catalog operation is invalid: {error}"))?;
    Ok(Some(spec))
}

fn contract_hash(contract: &ValueContractCatalog) -> Result<[u8; 32], String> {
    let material = value_contract_material(contract, VALUE_TYPE_LANGUAGE_VERSION)
        .map_err(|error| format!("value contract cannot be normalized: {error}"))?;
    let hash = value_contract_sha256(&material)
        .map_err(|error| format!("value contract cannot be hashed: {error}"))?;
    Ok(*hash.as_bytes())
}

fn parse_stable_semver(value: &str) -> Result<StableSemver, String> {
    if let Some(major) = value.strip_prefix('v') {
        return parse_semver_component(major)
            .map(|major| StableSemver::new(major, 0, 0))
            .ok_or_else(|| format!("invalid legacy connector version `{value}`"));
    }
    let mut components = value.split('.');
    let Some(major) = components.next().and_then(parse_semver_component) else {
        return Err(format!("invalid connector SemVer `{value}`"));
    };
    let Some(minor) = components.next().and_then(parse_semver_component) else {
        return Err(format!("invalid connector SemVer `{value}`"));
    };
    let Some(patch) = components.next().and_then(parse_semver_component) else {
        return Err(format!("invalid connector SemVer `{value}`"));
    };
    if components.next().is_some() {
        return Err(format!("invalid connector SemVer `{value}`"));
    }
    Ok(StableSemver::new(major, minor, patch))
}

fn parse_semver_component(value: &str) -> Option<u32> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    value.parse().ok()
}

fn compile_effect(effect: &ConnectorEffect) -> Result<OperationEffect, String> {
    match effect {
        ConnectorEffect::ReadOnly(_) => Ok(OperationEffect::ReadOnly),
        ConnectorEffect::ProviderIdempotent {
            provider_idempotent,
        } => Ok(OperationEffect::ProviderIdempotent {
            side_effect_steps: provider_idempotent
                .side_effect_steps
                .iter()
                .map(|step| {
                    Ok(ProviderIdempotentStep {
                        step: CompiledStepId::parse(&step.step)
                            .map_err(|_| "side-effect step is not a canonical ABI ID".to_owned())?,
                        fixed_binding: FixedIdempotencyBinding::Header {
                            name: step.fixed_binding.header.to_ascii_lowercase(),
                        },
                        scope: step.scope.clone(),
                        minimum_retention_ms: nonzero_u64(
                            step.minimum_retention_ms,
                            "minimum idempotency retention",
                        )?,
                        clock_safety_margin_ms: nonzero_u64(
                            step.clock_safety_margin_ms,
                            "idempotency clock margin",
                        )?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        }),
    }
}

fn compile_error_map(error_map: &donat_metadata::ConnectorErrorMap) -> Result<ErrorMap, String> {
    let mut fallback = CompleteErrorFallback {
        transport: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Transport,
            "connector_transport",
        )?,
        timeout: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Timeout,
            "connector_timeout",
        )?,
        http_429: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Http429,
            "connector_rate_limited",
        )?,
        http_5xx: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Http5xx,
            "connector_unavailable",
        )?,
        authentication: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Authentication,
            "connector_authentication",
        )?,
        validation: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Validation,
            "connector_validation",
        )?,
        permanent: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Permanent,
            "connector_permanent",
        )?,
        invariant: generic_error_action(
            donat_connector_abi::ConnectorErrorClass::Invariant,
            "connector_invariant",
        )?,
    };
    let compiled_fallback = metadata_error_action(&error_map.fallback)?;
    match error_map.fallback.class_ {
        ConnectorErrorClass::Transport => fallback.transport = compiled_fallback,
        ConnectorErrorClass::Timeout => fallback.timeout = compiled_fallback,
        ConnectorErrorClass::Http429 => fallback.http_429 = compiled_fallback,
        ConnectorErrorClass::Http5xx => fallback.http_5xx = compiled_fallback,
        ConnectorErrorClass::Authentication => fallback.authentication = compiled_fallback,
        ConnectorErrorClass::Validation => fallback.validation = compiled_fallback,
        ConnectorErrorClass::Permanent => fallback.permanent = compiled_fallback,
        ConnectorErrorClass::Invariant => fallback.invariant = compiled_fallback,
    }

    let rules = error_map
        .rules
        .iter()
        .flat_map(|rule| {
            rule.statuses.iter().copied().map(move |status| {
                Ok(ErrorRule {
                    matcher: ErrorMatcher::Status(StatusRange {
                        minimum: status,
                        maximum: status,
                    }),
                    action: ErrorAction::try_new(
                        connector_error_class(&rule.class_),
                        &rule.code,
                        "connector request failed",
                        RetryAfterPolicy::Never,
                        Vec::new(),
                    )
                    .map_err(|error| format!("invalid connector error rule: {error}"))?,
                })
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ErrorMap { rules, fallback })
}

fn metadata_error_action(error: &ConnectorError) -> Result<ErrorAction, String> {
    ErrorAction::try_new(
        connector_error_class(&error.class_),
        &error.code,
        "connector request failed",
        RetryAfterPolicy::Never,
        Vec::new(),
    )
    .map_err(|error| format!("invalid connector fallback error: {error}"))
}

fn generic_error_action(
    class: donat_connector_abi::ConnectorErrorClass,
    code: &str,
) -> Result<ErrorAction, String> {
    ErrorAction::try_new(
        class,
        code,
        "connector request failed",
        RetryAfterPolicy::Never,
        Vec::new(),
    )
    .map_err(|error| format!("invalid built-in connector fallback: {error}"))
}

fn connector_error_class(class: &ConnectorErrorClass) -> donat_connector_abi::ConnectorErrorClass {
    match class {
        ConnectorErrorClass::Transport => donat_connector_abi::ConnectorErrorClass::Transport,
        ConnectorErrorClass::Timeout => donat_connector_abi::ConnectorErrorClass::Timeout,
        ConnectorErrorClass::Http429 => donat_connector_abi::ConnectorErrorClass::Http429,
        ConnectorErrorClass::Http5xx => donat_connector_abi::ConnectorErrorClass::Http5xx,
        ConnectorErrorClass::Authentication => {
            donat_connector_abi::ConnectorErrorClass::Authentication
        }
        ConnectorErrorClass::Validation => donat_connector_abi::ConnectorErrorClass::Validation,
        ConnectorErrorClass::Permanent => donat_connector_abi::ConnectorErrorClass::Permanent,
        ConnectorErrorClass::Invariant => donat_connector_abi::ConnectorErrorClass::Invariant,
    }
}

fn body_bindings(value: &serde_json::Value) -> Vec<String> {
    fn visit(value: &serde_json::Value, seen: &mut BTreeSet<String>, bindings: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields)
                if fields.len() == 1 && fields.get("input").is_some() =>
            {
                if let Some(name) = fields.get("input").and_then(serde_json::Value::as_str)
                    && seen.insert(name.to_owned())
                {
                    bindings.push(name.to_owned());
                }
            }
            serde_json::Value::Object(fields) => {
                for value in fields.values() {
                    visit(value, seen, bindings);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, seen, bindings);
                }
            }
            _ => {}
        }
    }

    let mut seen = BTreeSet::new();
    let mut bindings = Vec::new();
    visit(value, &mut seen, &mut bindings);
    bindings
}

fn rate_refill_interval_ms(permits: u32, period: &str) -> Result<NonZeroU64, String> {
    let permits = NonZeroU64::new(u64::from(permits))
        .ok_or_else(|| "rate permits must be nonzero".to_owned())?;
    let (number, multiplier) = match period.chars().last() {
        Some('s') => (&period[..period.len() - 1], 1_000_u64),
        Some('m') => (&period[..period.len() - 1], 60_000_u64),
        Some('h') => (&period[..period.len() - 1], 3_600_000_u64),
        _ => return Err("rate period must end in s, m, or h".to_owned()),
    };
    let period_ms = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| "rate period is invalid or overflowed".to_owned())?;
    let rounded = period_ms
        .checked_add(permits.get() - 1)
        .ok_or_else(|| "rate refill interval overflowed".to_owned())?
        / permits.get();
    nonzero_u64(rounded, "rate refill interval")
}

fn nonzero_u32(value: u32, field: &str) -> Result<NonZeroU32, String> {
    NonZeroU32::new(value).ok_or_else(|| format!("{field} must be nonzero"))
}

fn nonzero_u64(value: u64, field: &str) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| format!("{field} must be nonzero"))
}

fn u64_to_nonzero_u32(value: u64, field: &str) -> Result<NonZeroU32, String> {
    let value = u32::try_from(value).map_err(|_| format!("{field} exceeds u32"))?;
    nonzero_u32(value, field)
}

enum NamedDeclaration<'metadata> {
    Rule(&'metadata RuleTypeDeclaration),
    CustomObject(&'metadata [CustomTypeField]),
    CustomEnum(&'metadata [EnumValue]),
    CustomScalar,
}

struct ContractDeclarations<'metadata> {
    declarations: BTreeMap<&'metadata str, NamedDeclaration<'metadata>>,
}

impl<'metadata> ContractDeclarations<'metadata> {
    fn new(metadata: &'metadata Metadata) -> Result<Self, String> {
        let mut declarations = BTreeMap::new();
        for declaration in &metadata.rules.types {
            insert_declaration(
                &mut declarations,
                &declaration.name,
                NamedDeclaration::Rule(declaration),
            )?;
        }
        for declaration in &metadata.custom_types.input_objects {
            insert_declaration(
                &mut declarations,
                &declaration.name,
                NamedDeclaration::CustomObject(&declaration.fields),
            )?;
        }
        for declaration in &metadata.custom_types.objects {
            insert_declaration(
                &mut declarations,
                &declaration.name,
                NamedDeclaration::CustomObject(&declaration.fields),
            )?;
        }
        for declaration in &metadata.custom_types.enums {
            insert_declaration(
                &mut declarations,
                &declaration.name,
                NamedDeclaration::CustomEnum(&declaration.values),
            )?;
        }
        for declaration in &metadata.custom_types.scalars {
            insert_declaration(
                &mut declarations,
                &declaration.name,
                NamedDeclaration::CustomScalar,
            )?;
        }
        Ok(Self { declarations })
    }

    fn contract<'field>(
        &self,
        fields: impl Iterator<Item = (&'field str, &'field str)>,
    ) -> Result<ValueContractCatalog, String> {
        let mut builder = ContractBuilder {
            declarations: self,
            named_objects: BTreeMap::new(),
            visiting: BTreeSet::new(),
        };
        let roots = fields
            .map(|(name, source)| {
                let type_ref = builder.type_ref(source)?;
                Ok((
                    name.to_owned(),
                    ValueContractField {
                        required: !type_ref.nullable,
                        type_ref,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        Ok(ValueContractCatalog {
            roots,
            named_objects: builder.named_objects,
        })
    }
}

fn insert_declaration<'metadata>(
    declarations: &mut BTreeMap<&'metadata str, NamedDeclaration<'metadata>>,
    name: &'metadata str,
    declaration: NamedDeclaration<'metadata>,
) -> Result<(), String> {
    if declarations.insert(name, declaration).is_some() {
        return Err(format!(
            "connector contract type `{name}` is declared more than once"
        ));
    }
    Ok(())
}

struct ContractBuilder<'declaration, 'metadata> {
    declarations: &'declaration ContractDeclarations<'metadata>,
    named_objects: BTreeMap<String, ValueObjectContract>,
    visiting: BTreeSet<String>,
}

impl ContractBuilder<'_, '_> {
    fn type_ref(&mut self, source: &str) -> Result<TypeRef, String> {
        let mut type_ref =
            TypeRef::parse(source).map_err(|error| format!("invalid contract type: {error}"))?;
        self.resolve_type_ref(&mut type_ref)?;
        Ok(type_ref)
    }

    fn resolve_type_ref(&mut self, type_ref: &mut TypeRef) -> Result<(), String> {
        match &mut type_ref.value_type {
            ValueType::List { element } => self.resolve_type_ref(element),
            ValueType::Ref { name } if name == "bigint" => {
                type_ref.value_type = ValueType::Scalar {
                    scalar: ValueScalar::Int64,
                };
                Ok(())
            }
            ValueType::Ref { name } => match self.declarations.declarations.get(name.as_str()) {
                Some(NamedDeclaration::Rule(declaration)) => {
                    let body_count = usize::from(declaration.object.is_some())
                        + usize::from(declaration.enum_values.is_some())
                        + usize::from(declaration.opaque_json.is_some());
                    if body_count != 1 {
                        return Err(format!(
                            "connector contract type `{name}` requires exactly one body"
                        ));
                    }
                    if let Some(symbols) = &declaration.enum_values {
                        if symbols.is_empty() {
                            return Err(format!(
                                "connector contract enum `{name}` cannot be empty"
                            ));
                        }
                        type_ref.value_type = ValueType::Enum {
                            name: name.clone(),
                            values: symbols.clone(),
                        };
                        Ok(())
                    } else if declaration.opaque_json.is_some() {
                        type_ref.value_type = ValueType::Scalar {
                            scalar: ValueScalar::Json,
                        };
                        Ok(())
                    } else {
                        let name = name.clone();
                        self.add_named_object(&name)
                    }
                }
                Some(NamedDeclaration::CustomEnum(values)) => {
                    if values.is_empty() {
                        return Err(format!("connector contract enum `{name}` cannot be empty"));
                    }
                    type_ref.value_type = ValueType::Enum {
                        name: name.clone(),
                        values: values.iter().map(|value| value.value.clone()).collect(),
                    };
                    Ok(())
                }
                Some(NamedDeclaration::CustomScalar) => {
                    type_ref.value_type = ValueType::Scalar {
                        scalar: ValueScalar::Custom { name: name.clone() },
                    };
                    Ok(())
                }
                Some(NamedDeclaration::CustomObject(_)) => {
                    let name = name.clone();
                    self.add_named_object(&name)
                }
                None => Err(format!("unknown connector contract type `{name}`")),
            },
            ValueType::Scalar { .. } | ValueType::Enum { .. } => Ok(()),
            ValueType::Object { fields } => {
                for field in fields.values_mut() {
                    self.resolve_type_ref(&mut field.type_ref)?;
                }
                Ok(())
            }
        }
    }

    fn add_named_object(&mut self, name: &str) -> Result<(), String> {
        if self.named_objects.contains_key(name) || !self.visiting.insert(name.to_owned()) {
            return Ok(());
        }
        let fields = match self.declarations.declarations.get(name) {
            Some(NamedDeclaration::Rule(declaration)) => declaration
                .object
                .as_ref()
                .ok_or_else(|| format!("connector contract type `{name}` is not an object"))?
                .iter()
                .map(|(field_name, source)| (field_name.as_str(), source.as_str()))
                .collect::<Vec<_>>(),
            Some(NamedDeclaration::CustomObject(fields)) => fields
                .iter()
                .map(|field| (field.name.as_str(), field.type_.as_str()))
                .collect(),
            _ => return Err(format!("connector contract type `{name}` is not an object")),
        };
        let fields = fields
            .into_iter()
            .map(|(field_name, source)| {
                let type_ref = self.type_ref(source)?;
                Ok((
                    field_name.to_owned(),
                    ValueContractField {
                        required: !type_ref.nullable,
                        type_ref,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        self.visiting.remove(name);
        self.named_objects
            .insert(name.to_owned(), ValueObjectContract { fields });
        Ok(())
    }
}
