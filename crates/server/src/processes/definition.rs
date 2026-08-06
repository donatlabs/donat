//! Durable Process compilation, as the server sees it.
//!
//! The compiler itself lives in `donat-processes` because the embedded wasm
//! core has to run it too: a command that starts a Process needs that
//! Process's effect contract to compile, and a contract derived differently
//! in wasm than in the engine would let the two disagree about what a command
//! may start.
//!
//! What stays here is the one binding that genuinely belongs to the server —
//! resolving connector operations and triggers out of the live
//! `ConnectorRegistry`, which the core does not have.

pub use donat_processes::*;

use donat_connector_abi::{OperationId, TriggerId};

use crate::connectors::ConnectorRegistry;

impl ProcessConnectorCatalog for ConnectorRegistry {
    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String> {
        let operation_id = OperationId::parse(operation)
            .map_err(|_| format!("connector operation `{operation}` is not a canonical ABI ID"))?;
        let Some(spec) = self.operation_spec_handle(source, instance, operation_id) else {
            return Ok(None);
        };
        let Some(deployment_fingerprint) = self.configuration_fingerprint(instance, operation)
        else {
            return Err(format!(
                "connector operation `{source}.{instance}.{operation}` has no deployment fingerprint"
            ));
        };
        Ok(Some(ResolvedProcessConnectorOperation {
            spec,
            deployment_fingerprint: deployment_fingerprint.to_owned(),
            serialization_key_input: self
                .serialization_key_input(instance, operation)
                .map(str::to_owned),
        }))
    }

    fn connector_trigger(
        &self,
        source: &str,
        instance: &str,
        trigger: &str,
    ) -> Result<Option<ResolvedProcessConnectorTrigger>, String> {
        let trigger_id = TriggerId::parse(trigger)
            .map_err(|_| format!("connector trigger `{trigger}` is not a canonical ABI ID"))?;
        let Some(spec) = self.trigger_spec_handle(source, instance, trigger_id) else {
            return Ok(None);
        };
        let Some(deployment_fingerprint) =
            self.trigger_configuration_fingerprint(instance, trigger_id)
        else {
            return Err(format!(
                "connector trigger `{source}.{instance}.{trigger}` has no deployment fingerprint"
            ));
        };
        Ok(Some(ResolvedProcessConnectorTrigger {
            spec,
            deployment_fingerprint: deployment_fingerprint.to_owned(),
        }))
    }
}
