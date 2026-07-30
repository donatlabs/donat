use donat_connector_abi::{CompiledStepId, ConnectorIo, TypedBindings};
use donat_connector_catalog::CompiledStepSpec;

fn accepts_connector_io_step(_: &dyn ConnectorIo, _: CompiledStepId, _: TypedBindings) {}

#[test]
fn catalog_descriptor_ids_match_connector_io() {
    fn assert_step_type(_: CompiledStepId) {}
    let step = CompiledStepSpec::minimal_for_identity(CompiledStepId::literal("request"));
    assert_step_type(step.step);
    let _ = accepts_connector_io_step;
}
