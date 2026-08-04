use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use donat_metadata::{Metadata, SourceKind};
use graphql_parser::query::{Definition, OperationDefinition};
use serde::Deserialize;
use serde_json::Value as Json;

const REVISION: &str = "c780834e50f53e5b4e94f1f33e88748a443f98ec";
const PINNED_OPERATIONS: [&str; 12] = [
    "AnalyticsDocumentDailyStats",
    "AnalyticsWorkflowExecutions",
    "AnalyticsErrors",
    "AnalyticsCodeLifecycleEvents",
    "AnalyticsAggregationOperations",
    "AnalyticsDashboardStats",
    "ApplicationLogsList",
    "DocumentIntegrationRequests",
    "L2JobEvents",
    "L2DeviceEvents",
    "L2TrafficLogs",
    "L2ProductionEvents",
];

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    source_paths: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    operation: String,
    source_path: Option<String>,
    query: String,
    sha256: String,
    pinned: bool,
    role: String,
    session: BTreeMap<String, String>,
    variables: Json,
    expected: String,
    #[serde(default)]
    expect_no_clickhouse_data_sql: bool,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../conformance/fixtures/tandt_clickhouse")
}

fn operation_names(query: &str) -> BTreeSet<String> {
    graphql_parser::parse_query::<String>(query)
        .expect("contract query parses")
        .definitions
        .into_iter()
        .filter_map(|definition| match definition {
            Definition::Operation(OperationDefinition::Query(query)) => query.name,
            Definition::Operation(OperationDefinition::Mutation(mutation)) => mutation.name,
            Definition::Operation(OperationDefinition::Subscription(subscription)) => {
                subscription.name
            }
            Definition::Operation(OperationDefinition::SelectionSet(_))
            | Definition::Fragment(_) => None,
        })
        .collect()
}

#[test]
fn pinned_contract_manifest_is_complete_and_byte_stable() {
    let manifest: Manifest = serde_json::from_str(include_str!(
        "../../conformance/fixtures/tandt_clickhouse/manifest.json"
    ))
    .expect("manifest parses");
    assert_eq!(manifest.revision, REVISION);
    assert_eq!(manifest.source_paths.len(), 7);

    let pinned = manifest
        .cases
        .iter()
        .filter(|case| case.pinned)
        .map(|case| case.operation.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(pinned, PINNED_OPERATIONS.into_iter().collect());

    let root = fixture_root();
    for case in &manifest.cases {
        let query = std::fs::read(root.join(&case.query)).expect("read query fixture");
        assert_eq!(sha256_hex(&query), case.sha256, "{} hash", case.operation);
        let query_text = std::str::from_utf8(&query).expect("query is UTF-8");
        assert!(
            operation_names(query_text).contains(&case.operation),
            "{} missing from {}",
            case.operation,
            case.query
        );

        let expected =
            std::fs::read_to_string(root.join(&case.expected)).expect("read raw response fixture");
        let compact = expected.strip_suffix('\n').unwrap_or(&expected);
        assert!(
            !compact.contains('\n'),
            "{} response is compact",
            case.operation
        );
        serde_json::from_str::<Json>(compact).expect("raw response is JSON");

        if case.pinned {
            let source = case.source_path.as_deref().expect("pinned source path");
            assert!(manifest.source_paths.iter().any(|path| path == source));
        } else {
            assert!(case.source_path.is_none(), "non-pinned source attribution");
        }
        assert_ne!(case.role, "admin", "there is no admin data role");
        assert!(case.variables.is_object());
        assert!(
            case.session
                .keys()
                .all(|name| name.starts_with("x-hasura-")),
            "session fixtures use Hasura-compatible claim names"
        );
    }

    let unsafe_dashboard = manifest
        .cases
        .iter()
        .find(|case| case.operation == "AnalyticsDashboardStats")
        .expect("unsafe dashboard case");
    assert!(unsafe_dashboard.expect_no_clickhouse_data_sql);
    assert_eq!(
        unsafe_dashboard
            .variables
            .as_object()
            .expect("dashboard variables")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["company_id"]
    );
    let unsafe_query = std::fs::read_to_string(root.join(&unsafe_dashboard.query)).unwrap();
    assert!(unsafe_query.contains("\"now() - interval '30 days'\""));
    assert!(unsafe_query.contains("\"now() - interval '7 days'\""));

    let safe_dashboard = manifest
        .cases
        .iter()
        .find(|case| case.operation == "AnalyticsDashboardStatsSafe")
        .expect("safe dashboard case");
    let safe_query = std::fs::read_to_string(root.join(&safe_dashboard.query)).unwrap();
    assert!(safe_query.contains("$document_since: date!"));
    assert!(safe_query.contains("$code_events_since: timestamp!"));
    assert!(!safe_query.contains("now()"));

    let workflow = manifest
        .cases
        .iter()
        .find(|case| case.operation == "AnalyticsWorkflowExecutions")
        .unwrap();
    assert_eq!(
        workflow.variables.pointer("/where/workflow_type/_like"),
        Some(&Json::String("Document%".to_string()))
    );
    assert!(workflow.variables.pointer("/where/name").is_none());

    let aggregation = manifest
        .cases
        .iter()
        .find(|case| case.operation == "AnalyticsAggregationOperations")
        .unwrap();
    assert!(aggregation.variables.get("offset").is_none());
    let aggregation_query = std::fs::read_to_string(root.join(&aggregation.query)).unwrap();
    assert!(!aggregation_query.contains("$offset"));
}

#[test]
fn metadata_matches_the_tandt_multi_database_security_shape() {
    let raw: Json = serde_json::from_str(include_str!("fixtures/tandt_clickhouse_metadata.json"))
        .expect("metadata JSON parses");
    assert_eq!(raw["x_tandt_contract"]["revision"], REVISION);

    let metadata: Metadata = serde_json::from_value(raw).expect("metadata deserializes");
    assert_eq!(metadata.sources.len(), 2);
    let default = metadata
        .sources
        .iter()
        .find(|source| source.name == "default")
        .expect("default source");
    assert_eq!(default.kind, SourceKind::Postgres);

    let clickhouse = metadata
        .sources
        .iter()
        .find(|source| source.name == "clickhouse")
        .expect("ClickHouse source");
    assert_eq!(clickhouse.kind, SourceKind::Clickhouse);
    let template = clickhouse.configuration.extra["template"]
        .as_str()
        .expect("Hasura configuration.template");
    for variable in [
        "CLICKHOUSE_HASURA_URL",
        "CLICKHOUSE_HASURA_USERNAME",
        "CLICKHOUSE_HASURA_PASSWORD",
    ] {
        assert!(template.contains(variable));
    }

    let databases = clickhouse
        .tables
        .iter()
        .map(|table| table.table.schema())
        .collect::<BTreeSet<_>>();
    assert_eq!(databases, BTreeSet::from(["analytics", "logs"]));
    assert_eq!(clickhouse.tables.len(), 12);

    for table in &clickhouse.tables {
        assert!(!table.select_permissions.is_empty(), "{}", table.table);
        for permission in &table.select_permissions {
            assert_ne!(permission.role, "admin");
            assert!(
                matches!(
                    permission.role.as_str(),
                    "company" | "company-admin" | "l2-executor"
                ),
                "unexpected role {} on {}",
                permission.role,
                table.table
            );
        }
    }

    let application_logs = clickhouse
        .tables
        .iter()
        .find(|table| table.table.name() == "application_logs")
        .unwrap();
    let columns = &application_logs.select_permissions[0].permission.columns;
    let encoded = serde_json::to_value(columns).unwrap();
    assert!(encoded.to_string().contains("context"));

    for table in clickhouse
        .tables
        .iter()
        .filter(|table| table.table.schema() == "analytics")
    {
        let filter = &table.select_permissions[0].permission.filter;
        assert_eq!(
            filter.pointer("/company_id/_eq"),
            Some(&Json::String("X-Hasura-Company-Id".to_string())),
            "{} company filter",
            table.table
        );
    }
}

#[test]
fn source_query_bundle_contains_all_pinned_and_sidecar_operations() {
    let query = include_str!("fixtures/tandt_clickhouse_queries.graphql");
    let names = operation_names(query);
    let mut expected = PINNED_OPERATIONS
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    expected.extend([
        "AnalyticsDashboardStatsSafe".to_string(),
        "ClickHouseComplexValues".to_string(),
        "MixedPostgresClickHouse".to_string(),
    ]);
    assert_eq!(names, expected);
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let sum1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(sum1);
            d = c;
            c = b;
            b = a;
            a = sum0.wrapping_add(sum1);
        }
        for (slot, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    hash.iter().map(|word| format!("{word:08x}")).collect()
}
