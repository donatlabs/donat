use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;

use crate::{
    BoundedString, BoundedTransportResponse, CapabilityId, CompiledStepId, ConnectorFailure,
    ConnectorId, Hash256, OperationId, TypedBindings,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ProcessorControl: Send + Sync {
    fn check(&self) -> Result<(), ConnectorFailure>;
}

pub trait ConnectorIo: Send + Sync {
    fn call<'a>(
        &'a self,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>>;
}

pub struct ProcessorContext<'a> {
    pub connector: &'a ConnectorId,
    pub operation: &'a OperationId,
    pub logical_activity_id: &'a BoundedString,
    pub idempotency_identity: &'a BoundedString,
    pub request_fingerprint: &'a Hash256,
    pub capabilities: &'a [CapabilityId],
    pub control: &'a dyn ProcessorControl,
}
