//! REST endpoint conformance: `rest_endpoints` metadata maps an HTTP
//! method + URL template to a saved GraphQL query (in `query_collections`),
//! served under `/api/rest/<url>`. The handler builds GraphQL variables from
//! the request (path > query string > body) and runs the saved query through
//! the normal pipeline; the response is the unwrapped GraphQL `data` object.

use donat_conformance::{Suite, Transport};

const REST: &str = "rest";

#[test]
fn rest_endpoints() {
    let s = Suite::new("rest_endpoints").start();
    s.setup_v1q(&format!("{REST}/setup.yaml"));

    for f in [
        "get_pet_by_id.yaml",
        "list_pets_limit.yaml",
        "create_pet.yaml",
        "method_not_allowed.yaml",
        "unknown_endpoint.yaml",
    ] {
        s.check_query_f(&format!("{REST}/{f}"), Transport::Http);
    }

    s.teardown_v1q(&format!("{REST}/teardown.yaml"));
}
