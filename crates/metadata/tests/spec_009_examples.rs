//! The spec's own YAML must load through the real types. A spec whose
//! examples do not parse documents a format that does not exist.

use donat_metadata::RestEndpoint;

#[test]
fn the_spec_example_deserializes() {
    let spec = include_str!("../../../specs/009-authenticated-endpoints-and-webhooks.md");
    let block = spec
        .split("```yaml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the spec has a yaml example");
    let endpoints: Vec<RestEndpoint> =
        serde_yaml::from_str(block.trim_start_matches("rest_endpoints:\n"))
            .expect("the spec's rest_endpoints example parses through the real types");
    assert_eq!(endpoints.len(), 1);
    let auth = endpoints[0].authenticate.as_ref().expect("authenticated");
    assert_eq!(auth.run_as, "billing");
}
