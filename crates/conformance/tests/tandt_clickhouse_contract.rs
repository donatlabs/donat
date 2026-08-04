use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use donat_conformance::{BackendId, Suite};
use donat_metadata::{Metadata, QualifiedTable, TableConfiguration};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value as Json, json};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize)]
struct Manifest {
    revision: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    operation: String,
    query: String,
    sha256: String,
    role: String,
    session: BTreeMap<String, String>,
    variables: Json,
    expected: String,
    #[serde(default)]
    expect_no_clickhouse_data_sql: bool,
}

struct ClickhouseDatabases {
    admin_url: String,
    names: Vec<String>,
}

impl Drop for ClickhouseDatabases {
    fn drop(&mut self) {
        let client = Client::new();
        for name in &self.names {
            let _ = client
                .post(&self.admin_url)
                .body(format!("DROP DATABASE IF EXISTS `{name}`"))
                .send();
        }
    }
}

struct ClickhouseConnection {
    admin_url: String,
    template_url: String,
    username: String,
    password: String,
}

fn fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tandt_clickhouse")
}

fn load_manifest() -> Manifest {
    serde_json::from_str(include_str!("../fixtures/tandt_clickhouse/manifest.json"))
        .expect("tandt contract manifest")
}

fn load_metadata() -> Metadata {
    serde_json::from_str(include_str!(
        "../../server/tests/fixtures/tandt_clickhouse_metadata.json"
    ))
    .expect("production-shaped tandt metadata")
}

fn clickhouse_connection() -> Option<ClickhouseConnection> {
    let configured = match std::env::var("CLICKHOUSE_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() => {
            panic!("CLICKHOUSE_URL must be set when DONAT_EXTERNAL_DB_TESTS=1")
        }
        Err(_) => return None,
    };
    let mut url = reqwest::Url::parse(&configured).expect("valid CLICKHOUSE_URL");
    let username = if url.username().is_empty() {
        "default".to_string()
    } else {
        url.username().to_string()
    };
    let password = url.password().unwrap_or_default().to_string();
    url.set_username("").expect("clear ClickHouse username");
    url.set_password(None).expect("clear ClickHouse password");
    let retained = url
        .query_pairs()
        .filter(|(key, _)| key != "database")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(retained);
    let template_url = url.to_string();

    let mut admin = url;
    admin
        .set_username(&username)
        .expect("set ClickHouse username");
    admin
        .set_password(Some(&password))
        .expect("set ClickHouse password");
    Some(ClickhouseConnection {
        admin_url: admin.to_string(),
        template_url,
        username,
        password,
    })
}

fn execute_clickhouse(client: &Client, url: &str, sql: &str) -> String {
    let mut url = reqwest::Url::parse(url).expect("valid ClickHouse endpoint");
    url.query_pairs_mut()
        .append_pair("allow_experimental_json_type", "1")
        .append_pair("enable_named_columns_in_function_tuple", "1");
    let response = client
        .post(url)
        .body(sql.to_string())
        .send()
        .unwrap_or_else(|error| panic!("ClickHouse request failed for {sql}: {error}"));
    let status = response.status();
    let body = response.text().unwrap_or_default();
    assert!(status.is_success(), "ClickHouse failed: {sql}\n{body}");
    body
}

fn provision_clickhouse(
    connection: &ClickhouseConnection,
) -> (ClickhouseDatabases, String, String) {
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    );
    let analytics = format!("donat_tandt_analytics_{suffix}");
    let logs = format!("donat_tandt_logs_{suffix}");
    let databases = ClickhouseDatabases {
        admin_url: connection.admin_url.clone(),
        names: vec![analytics.clone(), logs.clone()],
    };
    let client = Client::new();
    for database in &databases.names {
        execute_clickhouse(
            &client,
            &connection.admin_url,
            &format!("CREATE DATABASE `{database}`"),
        );
    }

    let setup: Vec<String> =
        serde_json::from_str(include_str!("../fixtures/tandt_clickhouse/setup.json"))
            .expect("ClickHouse setup fixture");
    for statement in setup {
        let statement = statement
            .replace("{{analytics}}", &analytics)
            .replace("{{logs}}", &logs);
        execute_clickhouse(&client, &connection.admin_url, &statement);
    }
    (databases, analytics, logs)
}

fn isolated_metadata(analytics: &str, logs: &str) -> Metadata {
    let mut metadata = load_metadata();
    let clickhouse = metadata
        .sources
        .iter_mut()
        .find(|source| source.name == "clickhouse")
        .expect("ClickHouse source");
    for table in &mut clickhouse.tables {
        let fixture_schema = table.table.schema().to_string();
        let fixture_name = table.table.name().to_string();
        let mut configuration = table.configuration.clone().unwrap_or_default();
        let root_name = configuration
            .custom_name
            .clone()
            .unwrap_or_else(|| format!("{fixture_schema}_{fixture_name}"));
        let database = match fixture_schema.as_str() {
            "analytics" => analytics,
            "logs" => logs,
            other => panic!("unexpected ClickHouse fixture database {other}"),
        };
        table.table = QualifiedTable::Qualified {
            schema: database.to_string(),
            name: fixture_name,
        };
        configuration.custom_name = Some(root_name);
        table.configuration = Some(TableConfiguration { ..configuration });
    }
    metadata
}

fn data_query_count(connection: &ClickhouseConnection, analytics: &str, logs: &str) -> u64 {
    let client = Client::new();
    execute_clickhouse(&client, &connection.admin_url, "SYSTEM FLUSH LOGS");
    let sql = format!(
        "SELECT count() FROM system.query_log \
         WHERE type = 'QueryFinish' \
         AND (position(query, '`{analytics}`') > 0 OR position(query, '`{logs}`') > 0) \
         AND position(query, 'system.columns') = 0 \
         AND position(query, 'system.query_log') = 0 FORMAT TSVRaw"
    );
    execute_clickhouse(&client, &connection.admin_url, &sql)
        .trim()
        .parse()
        .expect("ClickHouse data query count")
}

fn post_raw(base_url: &str, case: &Case, query: &str) -> (u16, String) {
    let request_body = serde_json::to_vec(&json!({
        "query": query,
        "variables": case.variables,
        "operationName": case.operation
    }))
    .expect("serialize exact GraphQL request");
    let mut request = Client::new()
        .post(format!("{base_url}/v1/graphql"))
        .header("content-type", "application/json")
        .header("x-donat-role", &case.role)
        .body(request_body);
    for (name, value) in &case.session {
        request = request.header(name, value);
    }
    let response = request.send().expect("GraphQL request");
    let status = response.status().as_u16();
    let body = response.text().expect("raw GraphQL response");
    (status, body)
}

#[test]
fn tandt_clickhouse_operations_match_raw_hasura_contract() {
    let Some(connection) = clickhouse_connection() else {
        eprintln!("skipping tandt ClickHouse contract: CLICKHOUSE_URL is not set");
        return;
    };
    if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() {
        std::env::var("PG_URL").expect("PG_URL must be set when DONAT_EXTERNAL_DB_TESTS=1");
    }

    let manifest = load_manifest();
    assert_eq!(
        manifest.revision,
        "c780834e50f53e5b4e94f1f33e88748a443f98ec"
    );
    let (_databases, analytics, logs) = provision_clickhouse(&connection);
    let suite = Suite::new("tandt-clickhouse-contract")
        .backend(BackendId::Postgres)
        .initial_metadata(isolated_metadata(&analytics, &logs))
        .env("CLICKHOUSE_HASURA_URL", &connection.template_url)
        .env("CLICKHOUSE_HASURA_USERNAME", &connection.username)
        .env("CLICKHOUSE_HASURA_PASSWORD", &connection.password)
        .start();
    let mut postgres = postgres::Client::connect(suite.db_url(), postgres::NoTls)
        .expect("connect to mandatory Postgres fixture");
    postgres
        .batch_execute(
            "CREATE TABLE contract_probe (id bigint PRIMARY KEY, label text NOT NULL); \
             INSERT INTO contract_probe VALUES (1, 'postgres')",
        )
        .expect("seed mixed-source Postgres fixture");
    drop(postgres);
    let base_url = suite.base_url();
    let root = fixture_root();
    let mut failures = Vec::new();

    for case in &manifest.cases {
        let query_bytes = std::fs::read(root.join(&case.query)).expect("read query fixture");
        assert_eq!(
            sha256_hex(&query_bytes),
            case.sha256,
            "{} hash",
            case.operation
        );
        let query = std::str::from_utf8(&query_bytes).expect("query fixture UTF-8");
        let before = case
            .expect_no_clickhouse_data_sql
            .then(|| data_query_count(&connection, &analytics, &logs));
        let (status, actual) = post_raw(&base_url, case, query);
        let after = case
            .expect_no_clickhouse_data_sql
            .then(|| data_query_count(&connection, &analytics, &logs));
        let expected =
            std::fs::read_to_string(root.join(&case.expected)).expect("read expected raw response");
        let expected = expected.strip_suffix('\n').unwrap_or(&expected);

        let no_sql_ok = before
            .zip(after)
            .is_none_or(|(before, after)| before == after);
        if status == 200 && actual == expected && no_sql_ok {
            eprintln!("{}: PASS", case.operation);
        } else {
            eprintln!("{}: RED", case.operation);
            failures.push(format!(
                "{}: status={status}, no_data_sql={no_sql_ok}\nexpected: {expected}\nactual:   {actual}",
                case.operation
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "tandt ClickHouse compatibility gaps:\n\n{}",
        failures.join("\n\n")
    );
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
