//! Native conformance harness.
//!
//! Executes Hasura-derived YAML fixtures (`crates/conformance/fixtures`)
//! against a freshly spawned `dist-api` instance, replicating the semantics
//! of tests-py `check_query_f`: same fixture format (`url`, `status`,
//! `headers`, `query`, `response`, list-of-steps files, `!include`), same
//! response comparison (key order enforced inside `data`, order-insensitive
//! elsewhere), same legacy-Apollo websocket protocol.
//!
//! Each suite runs against its own Postgres database (created from the
//! admin connection in `PG_URL`), so suites are hermetic and parallel-safe.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value as Json, json};

// ---------------------------------------------------------------- fixtures

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Load a fixture YAML into JSON, resolving `!include <file>` (both the real
/// YAML tag and the quoted-string spelling hasura-cli produces) relative to
/// the including file.
pub fn load_fixture(path: &Path) -> Result<Json> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading fixture {}", path.display()))?;
    let v: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parsing fixture {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    yaml_to_json(&v, dir)
}

fn yaml_to_json(v: &serde_yaml::Value, dir: &Path) -> Result<Json> {
    use serde_yaml::Value as Y;
    Ok(match v {
        Y::Null => Json::Null,
        Y::Bool(b) => Json::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                json!(n.as_f64().unwrap())
            }
        }
        Y::String(s) => {
            if let Some(rest) = s.strip_prefix("!include ") {
                load_fixture(&dir.join(rest.trim()))?
            } else {
                Json::String(s.clone())
            }
        }
        Y::Sequence(xs) => Json::Array(
            xs.iter()
                .map(|x| yaml_to_json(x, dir))
                .collect::<Result<_>>()?,
        ),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let key = match k {
                    Y::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)?.trim().to_string(),
                };
                out.insert(key, yaml_to_json(val, dir)?);
            }
            Json::Object(out)
        }
        Y::Tagged(t) => {
            if t.tag.to_string().trim_start_matches('!') == "include" {
                let f = t
                    .value
                    .as_str()
                    .ok_or_else(|| anyhow!("!include expects a string"))?;
                load_fixture(&dir.join(f))?
            } else {
                yaml_to_json(&t.value, dir)?
            }
        }
    })
}

// -------------------------------------------------------------- comparison

/// Selection tree extracted from the fixture's GraphQL query: response-alias
/// -> nested selections (None for leaf fields). Used to replicate tests-py
/// `collapse_order_not_selset`: key order is enforced only among keys that
/// are part of the selection set; everything else (errors, jsonb column
/// values, ...) compares order-insensitively.
#[derive(Default)]
pub struct SelMap(std::collections::HashMap<String, Option<SelMap>>);

impl SelMap {
    fn contains_key(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
    fn get(&self, k: &str) -> Option<&Option<SelMap>> {
        self.0.get(k)
    }
}

pub fn sel_tree_from_query(query: &str) -> Option<SelMap> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection, SelectionSet};

    let doc = graphql_parser::parse_query::<String>(query).ok()?;
    let mut frags = std::collections::HashMap::new();
    for def in &doc.definitions {
        if let Definition::Fragment(f) = def {
            frags.insert(f.name.clone(), &f.selection_set);
        }
    }
    fn build<'a>(
        ss: &SelectionSet<'a, String>,
        frags: &std::collections::HashMap<String, &SelectionSet<'a, String>>,
    ) -> SelMap {
        let mut out = SelMap::default();
        for item in &ss.items {
            match item {
                Selection::Field(f) => {
                    let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
                    let child = if f.selection_set.items.is_empty() {
                        None
                    } else {
                        Some(build(&f.selection_set, frags))
                    };
                    out.0.insert(key, child);
                }
                Selection::FragmentSpread(fs) => {
                    if let Some(inner) = frags.get(&fs.fragment_name) {
                        out.0.extend(build(inner, frags).0);
                    }
                }
                Selection::InlineFragment(inf) => {
                    out.0.extend(build(&inf.selection_set, frags).0);
                }
            }
        }
        out
    }
    for def in &doc.definitions {
        let ss = match def {
            Definition::Operation(OperationDefinition::Query(q)) => &q.selection_set,
            Definition::Operation(OperationDefinition::Mutation(m)) => &m.selection_set,
            Definition::Operation(OperationDefinition::Subscription(s)) => &s.selection_set,
            Definition::Operation(OperationDefinition::SelectionSet(ss)) => ss,
            Definition::Fragment(_) => continue,
        };
        return Some(build(ss, &frags));
    }
    None
}

/// Deep comparison. `sel` carries the selection tree for the current level;
/// among keys present in the tree, the relative order in expected and actual
/// must match, and their children recurse with their sub-tree. Keys outside
/// the tree (and everything once `sel` is None) compare order-insensitively.
/// Numbers compare by value (1 == 1.0), like Python.
pub fn json_matches(exp: &Json, act: &Json, sel: Option<&SelMap>) -> bool {
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            if let Some(tree) = sel {
                let eseq: Vec<&String> = e.keys().filter(|k| tree.contains_key(*k)).collect();
                let aseq: Vec<&String> = a.keys().filter(|k| tree.contains_key(*k)).collect();
                if eseq != aseq {
                    return false;
                }
            }
            e.iter().all(|(k, ve)| {
                let child = sel.and_then(|t| t.get(k)).and_then(|c| c.as_ref());
                json_matches(ve, &a[k], child)
            })
        }
        (Json::Array(e), Json::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a.iter()).all(|(x, y)| json_matches(x, y, sel))
        }
        (Json::Number(e), Json::Number(a)) => {
            e == a || (e.as_f64().zip(a.as_f64()).is_some_and(|(x, y)| x == y))
        }
        _ => exp == act,
    }
}

/// Compare a full HTTP-level response: top-level object unordered, the
/// `data` subtree governed by the query's selection tree.
pub fn response_matches(exp: &Json, act: &Json, query_text: Option<&str>) -> bool {
    let tree = query_text.and_then(sel_tree_from_query);
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            e.iter().all(|(k, ve)| {
                let sel = if k == "data" { tree.as_ref() } else { None };
                json_matches(ve, &a[k], sel)
            })
        }
        _ => json_matches(exp, act, None),
    }
}

// ------------------------------------------------------------------ engine

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

static BUILD_ENGINE: Once = Once::new();

pub fn engine_binary() -> PathBuf {
    if let Ok(p) = std::env::var("DIST_API_BIN") {
        return PathBuf::from(p);
    }
    let bin = workspace_root().join("target/debug/dist-api");
    BUILD_ENGINE.call_once(|| {
        if !bin.exists() {
            let status = Command::new("cargo")
                .args(["build", "-p", "dist-server", "--bin", "dist-api"])
                .current_dir(workspace_root())
                .status()
                .expect("running cargo build");
            assert!(status.success(), "cargo build -p dist-server failed");
        }
    });
    bin
}

pub fn pg_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".into())
}

/// `postgresql://u:p@h:port/db` with the database swapped out.
fn with_db(admin_url: &str, db: &str) -> String {
    let (prefix, _) = admin_url
        .rsplit_once('/')
        .expect("PG_URL must contain a database path");
    format!("{prefix}/{db}")
}

fn create_suite_db(name: &str) -> Result<String> {
    let admin = pg_admin_url();
    let mut client = postgres::Client::connect(&admin, postgres::NoTls)
        .with_context(|| format!("connecting to {admin} (is the postgres container up?)"))?;
    client.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))?;
    client.batch_execute(&format!("CREATE DATABASE {name}"))?;
    Ok(with_db(&admin, name))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    Ws,
    Both,
}

pub struct Suite {
    name: String,
    env: Vec<(String, String)>,
    args: Vec<String>,
    admin_secret: Option<String>,
}

impl Suite {
    pub fn new(name: &str) -> Self {
        Suite {
            name: name.to_string(),
            env: vec![],
            args: vec![],
            admin_secret: None,
        }
    }

    /// Classes marked `@pytest.mark.admin_secret`: the engine gets
    /// HASURA_GRAPHQL_ADMIN_SECRET and every request carries the secret
    /// header (mirroring tests-py `add_auth`).
    pub fn admin_secret(mut self, secret: &str) -> Self {
        self.admin_secret = Some(secret.to_string());
        self.env.push((
            "HASURA_GRAPHQL_ADMIN_SECRET".to_string(),
            secret.to_string(),
        ));
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    pub fn arg(mut self, a: &str) -> Self {
        self.args.push(a.to_string());
        self
    }

    pub fn start(self) -> Running {
        let bin = engine_binary();
        let db_url =
            create_suite_db(&format!("conf_{}", self.name)).expect("creating suite database");
        let port = free_port();
        let log_dir = workspace_root().join("target/conformance-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log = std::fs::File::create(log_dir.join(format!("{}.log", self.name))).unwrap();

        let mut cmd = Command::new(&bin);
        cmd.arg("--port")
            .arg(port.to_string())
            .env("DIST_API_DATABASE_URL", &db_url)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        for a in &self.args {
            cmd.arg(a);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawning dist-api");

        let running = Running {
            name: self.name,
            base_url: format!("http://127.0.0.1:{port}"),
            ws_base: format!("ws://127.0.0.1:{port}"),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            child,
            admin_secret: self.admin_secret,
        };
        running.wait_healthy();
        // Fresh database: postgis used pervasively by fixtures. Concurrent
        // CREATE EXTENSION across databases races inside Postgres (shared
        // library/template locks) — serialize within this process and retry
        // to cover other test processes.
        static POSTGIS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = POSTGIS_LOCK.lock().unwrap();
        let mut last = (0u16, Json::Null);
        for _ in 0..10 {
            let (code, resp) = running.post(
                "/v1/query",
                &json!({"type":"run_sql","args":{"sql":"create extension if not exists postgis"}}),
                &running.auth_headers(vec![]),
            );
            if code < 300 {
                return running;
            }
            last = (code, resp);
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!(
            "postgis init failed [{}] after retries ({}): {}",
            running.name,
            last.0,
            pretty(&last.1)
        );
    }
}

pub struct Running {
    pub name: String,
    pub base_url: String,
    pub ws_base: String,
    http: reqwest::blocking::Client,
    child: Child,
    admin_secret: Option<String>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Running {
    fn wait_healthy(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = self.http.get(format!("{}/healthz", self.base_url)).send()
                && r.status().is_success()
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "engine for suite {} did not become healthy; see target/conformance-logs/{}.log",
                self.name,
                self.name
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn post(&self, path: &str, body: &Json, headers: &[(String, String)]) -> (u16, Json) {
        let mut req = self.http.post(format!("{}{path}", self.base_url)).json(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().expect("http request failed");
        let code = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Json::String(text));
        (code, body)
    }

    fn auth_headers(&self, mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
        if let Some(secret) = &self.admin_secret {
            headers.push(("X-Hasura-Admin-Secret".to_string(), secret.clone()));
        }
        headers
    }

    /// Apply a setup/teardown fixture: POST the whole document to `endpoint`,
    /// asserting success.
    pub fn apply(&self, rel: &str, endpoint: &str) {
        let path = fixture_root().join(rel);
        let body = load_fixture(&path).expect("loading setup fixture");
        let (code, resp) = self.post(endpoint, &body, &self.auth_headers(vec![]));
        assert!(
            code < 300,
            "[{}] setup {rel} via {endpoint} failed ({code}):\n{}",
            self.name,
            pretty(&resp)
        );
    }

    /// tests-py applies v2-style setup files only when they exist.
    pub fn apply_if_exists(&self, rel: &str, endpoint: &str) -> bool {
        if fixture_root().join(rel).exists() {
            self.apply(rel, endpoint);
            true
        } else {
            false
        }
    }

    pub fn setup_v1q(&self, rel: &str) {
        self.apply(rel, "/v1/query");
    }

    /// Best-effort teardown (pytest asserts these too, but our per-suite
    /// database makes teardown failures non-isolating; keep the assert to
    /// stay faithful).
    pub fn teardown_v1q(&self, rel: &str) {
        self.apply(rel, "/v1/query");
    }

    /// Replicates tests-py `check_query_f` for one fixture file.
    pub fn check_query_f(&self, rel: &str, transport: Transport) {
        let path = fixture_root().join(rel);
        let conf = load_fixture(&path).expect("loading test fixture");
        match conf {
            Json::Array(steps) => {
                for (i, step) in steps.iter().enumerate() {
                    self.run_conf(step, transport, &format!("{rel}[{i}]"));
                }
            }
            other => self.run_conf(&other, transport, rel),
        }
    }

    fn run_conf(&self, conf: &Json, transport: Transport, label: &str) {
        let url = conf["url"].as_str().expect("conf.url");
        let is_gql = url.ends_with("/graphql") || url.ends_with("/relay");
        match transport {
            Transport::Http => self.http_case(conf, label),
            Transport::Ws => {
                assert!(is_gql, "ws transport on non-graphql url in {label}");
                self.ws_case(conf, label);
            }
            Transport::Both => {
                self.http_case(conf, label);
                if is_gql {
                    self.ws_case(conf, label);
                }
            }
        }
    }

    fn conf_headers(conf: &Json) -> Vec<(String, String)> {
        conf.get("headers")
            .and_then(|h| h.as_object())
            .map(|h| {
                h.iter()
                    .map(|(k, v)| {
                        let val = match v {
                            Json::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn http_case(&self, conf: &Json, label: &str) {
        let url = conf["url"].as_str().unwrap();
        let headers = self.auth_headers(Self::conf_headers(conf));
        let exp_status = conf.get("status").and_then(Json::as_u64).unwrap_or(200) as u16;
        let method = conf.get("method").and_then(Json::as_str).unwrap_or("POST");

        let (code, resp) = match method {
            "GET" => {
                let mut req = self.http.get(format!("{}{url}", self.base_url));
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                let r = req.send().expect("http GET failed");
                let code = r.status().as_u16();
                let text = r.text().unwrap_or_default();
                (
                    code,
                    serde_json::from_str(&text).unwrap_or(Json::String(text)),
                )
            }
            _ => {
                let body = conf.get("query").or_else(|| conf.get("body")).cloned();
                self.post(url, &body.unwrap_or(Json::Null), &headers)
            }
        };

        assert_eq!(
            code, exp_status,
            "[{}] {label}: status mismatch (got {code}, want {exp_status})\nresponse:\n{}",
            self.name,
            pretty(&resp)
        );

        let query_text = conf_query_text(conf);
        if let Some(allowed) = conf.get("allowed_responses").and_then(Json::as_array) {
            let ok = allowed.iter().any(|a| {
                a.get("response")
                    .is_some_and(|exp| response_matches(exp, &resp, query_text))
            });
            assert!(
                ok,
                "[{}] {label}: response matched none of allowed_responses\nactual:\n{}",
                self.name,
                pretty(&resp)
            );
        } else if let Some(exp) = conf.get("response") {
            self.assert_response(exp, &resp, query_text, label);
        }
    }

    fn assert_response(&self, exp: &Json, act: &Json, query_text: Option<&str>, label: &str) {
        assert!(
            response_matches(exp, act, query_text),
            "[{}] {label}: response mismatch\nexpected:\n{}\nactual:\n{}",
            self.name,
            pretty(exp),
            pretty(act)
        );
    }

    /// Legacy Apollo graphql-ws: init({headers}) -> ack, start -> data|error
    /// (payload compared against the full expected HTTP response), then
    /// complete.
    fn ws_case(&self, conf: &Json, label: &str) {
        use tungstenite::Message;
        use tungstenite::client::IntoClientRequest;

        let url = conf["url"].as_str().unwrap();
        let exp = conf
            .get("response")
            .unwrap_or_else(|| panic!("[{label}] ws case without response"));
        let headers = self.auth_headers(Self::conf_headers(conf));
        let query = conf["query"].clone();

        let mut req = format!("{}{url}", self.ws_base)
            .into_client_request()
            .expect("ws request");
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "graphql-ws".parse().expect("protocol header"),
        );
        let (mut sock, _) = tungstenite::connect(req).expect("ws connect");

        let mut init_payload = Map::new();
        if !headers.is_empty() {
            init_payload.insert(
                "headers".into(),
                Json::Object(headers.into_iter().map(|(k, v)| (k, json!(v))).collect()),
            );
        }
        sock.send(Message::text(
            json!({"type":"connection_init","payload": init_payload}).to_string(),
        ))
        .unwrap();

        let frame = next_frame(&mut sock, &["connection_ack", "connection_error"], label);
        assert_eq!(
            frame["type"], "connection_ack",
            "[{label}] ws init failed: {}",
            pretty(&frame)
        );

        sock.send(Message::text(
            json!({"id":"hge_test","type":"start","payload": query}).to_string(),
        ))
        .unwrap();

        let frame = next_frame(&mut sock, &["data", "error"], label);
        let payload = &frame["payload"];
        let payload = if frame["type"] == "error" {
            // Legacy protocol error frames carry the bare error object.
            &json!({ "errors": [payload.clone()] })
        } else {
            payload
        };
        self.assert_response(exp, payload, conf_query_text(conf), &format!("{label} (ws)"));

        let has_errors = exp.get("errors").is_some() || exp.get("error").is_some();
        if !has_errors {
            let done = next_frame(&mut sock, &["complete"], label);
            assert_eq!(done["type"], "complete", "[{label}] expected complete");
        }
        let _ = sock.close(None);
    }
}

fn next_frame<S>(
    sock: &mut tungstenite::WebSocket<S>,
    wanted: &[&str],
    label: &str,
) -> Json
where
    S: Read + std::io::Write,
{
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "[{label}] timed out waiting for ws frame {wanted:?}"
        );
        let msg = sock.read().expect("ws read");
        if !msg.is_text() {
            continue;
        }
        let v: Json = serde_json::from_str(msg.to_text().unwrap()).expect("ws frame json");
        let t = v["type"].as_str().unwrap_or_default().to_string();
        if t == "ka" {
            continue;
        }
        if wanted.contains(&t.as_str()) || t == "error" || t == "connection_error" {
            return v;
        }
    }
}

fn conf_query_text(conf: &Json) -> Option<&str> {
    conf.get("query")?.get("query")?.as_str()
}

fn pretty(v: &Json) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

// -------------------------------------------------------------------- tests

/// Unit tests for the pure parts of the harness: fixture loading
/// (`!include`) and the tests-py-faithful response comparison. They need
/// neither Postgres nor a running engine, so they live in the lib target
/// (the `tests/` binaries require a database).
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ---------------------------------------------------- load_fixture

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dist_conformance_fixture_{tag}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn load_fixture_resolves_string_include_relative_to_file() {
        // The quoted-string spelling, resolved against the *including*
        // file's directory — including transitively from a subdirectory.
        let dir = tempdir("string");
        write(
            &dir,
            "suite/case.yaml",
            "setup: \"!include sub/inner.yaml\"\nname: top\n",
        );
        write(&dir, "suite/sub/inner.yaml", "deep: \"!include leaf.yaml\"\n");
        write(&dir, "suite/sub/leaf.yaml", "- 1\n- two\n");

        let v = load_fixture(&dir.join("suite/case.yaml")).unwrap();
        assert_eq!(v["name"], json!("top"));
        assert_eq!(v["setup"]["deep"], json!([1, "two"]));
    }

    #[test]
    fn load_fixture_resolves_real_yaml_tag_include() {
        let dir = tempdir("tag");
        write(&dir, "case.yaml", "steps: !include steps.yaml\n");
        write(&dir, "steps.yaml", "- url: /v1/graphql\n  status: 200\n");

        let v = load_fixture(&dir.join("case.yaml")).unwrap();
        assert_eq!(v["steps"][0]["url"], json!("/v1/graphql"));
        assert_eq!(v["steps"][0]["status"], json!(200));
    }

    #[test]
    fn load_fixture_missing_include_target_errors() {
        let dir = tempdir("missing");
        write(&dir, "case.yaml", "setup: \"!include nope.yaml\"\n");
        let err = load_fixture(&dir.join("case.yaml")).unwrap_err();
        assert!(format!("{err:#}").contains("nope.yaml"), "got: {err:#}");
    }

    #[test]
    fn load_fixture_preserves_numbers_and_non_string_keys() {
        let dir = tempdir("scalars");
        write(
            &dir,
            "case.yaml",
            "int: 5\nbig: 18446744073709551615\nfloat: 1.5\nmap:\n  1: one\n",
        );
        let v = load_fixture(&dir.join("case.yaml")).unwrap();
        assert_eq!(v["int"], json!(5));
        assert_eq!(v["big"], json!(18446744073709551615u64));
        assert_eq!(v["float"], json!(1.5));
        // Non-string YAML keys are stringified.
        assert_eq!(v["map"]["1"], json!("one"));
    }

    // ----------------------------------------- json/response matching

    #[test]
    fn numbers_coerce_across_int_and_float() {
        assert!(json_matches(&json!(1), &json!(1.0), None));
        assert!(json_matches(&json!(1.0), &json!(1), None));
        assert!(!json_matches(&json!(1), &json!(2.0), None));
        assert!(json_matches(&json!({"n": 1}), &json!({"n": 1.0}), None));
    }

    #[test]
    fn objects_without_selection_tree_compare_unordered() {
        let exp = json!({"a": 1, "b": 2});
        let act = json!({"b": 2, "a": 1});
        assert!(json_matches(&exp, &act, None));
    }

    #[test]
    fn object_key_set_mismatch_fails() {
        // Missing, extra, and renamed keys all fail even order-insensitively.
        assert!(!json_matches(&json!({"a": 1}), &json!({"a": 1, "b": 2}), None));
        assert!(!json_matches(&json!({"a": 1, "b": 2}), &json!({"a": 1}), None));
        assert!(!json_matches(&json!({"a": 1}), &json!({"b": 1}), None));
    }

    #[test]
    fn arrays_require_equal_length_and_order() {
        assert!(json_matches(&json!([1, 2]), &json!([1, 2]), None));
        assert!(!json_matches(&json!([1, 2]), &json!([2, 1]), None));
        assert!(!json_matches(&json!([1, 2]), &json!([1, 2, 3]), None));
    }

    #[test]
    fn data_key_order_is_enforced_per_selection_tree() {
        let query = "query { a b }";
        let exp = json!({"data": {"a": 1, "b": 2}});
        let in_order = json!({"data": {"a": 1, "b": 2}});
        let reordered = json!({"data": {"b": 2, "a": 1}});
        assert!(response_matches(&exp, &in_order, Some(query)));
        assert!(!response_matches(&exp, &reordered, Some(query)));
    }

    #[test]
    fn nested_selection_order_is_enforced_per_level() {
        let query = "query { items { x y } }";
        let exp = json!({"data": {"items": [{"x": 1, "y": 2}]}});
        let good = json!({"data": {"items": [{"x": 1, "y": 2}]}});
        let bad = json!({"data": {"items": [{"y": 2, "x": 1}]}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(!response_matches(&exp, &bad, Some(query)));
    }

    #[test]
    fn aliases_key_the_selection_tree() {
        // The response key is the alias; ordering is enforced on aliases.
        let query = "query { first: item { v } second: item { v } }";
        let exp = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
        let good = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
        let swapped = json!({"data": {"second": {"v": 2}, "first": {"v": 1}}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(!response_matches(&exp, &swapped, Some(query)));
    }

    #[test]
    fn fragment_spread_fields_join_the_selection_tree() {
        let query = "
            query { item { ...F } }
            fragment F on Item { p q }
        ";
        let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
        let good = json!({"data": {"item": {"p": 1, "q": 2}}});
        let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(
            !response_matches(&exp, &bad, Some(query)),
            "fragment fields must take part in order enforcement"
        );
    }

    #[test]
    fn inline_fragment_fields_join_the_selection_tree() {
        let query = "query { item { ... on Item { p q } } }";
        let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
        let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
        assert!(!response_matches(&exp, &bad, Some(query)));
    }

    #[test]
    fn jsonb_value_under_data_leaf_is_not_order_enforced() {
        // `payload` is a leaf field (no sub-selection): its object value is
        // a jsonb column, where Postgres does not guarantee key order.
        let query = "query { item { payload } }";
        let exp = json!({"data": {"item": {"payload": {"x": 1, "y": 2}}}});
        let act = json!({"data": {"item": {"payload": {"y": 2, "x": 1}}}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn keys_outside_the_selection_tree_are_not_order_enforced() {
        // Only keys present in the selection tree participate in the
        // relative-order check (collapse_order_not_selset semantics).
        let query = "query { a }";
        let exp = json!({"data": {"extra": 0, "a": 1}});
        let act = json!({"data": {"a": 1, "extra": 0}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn errors_compare_unordered() {
        // `errors` is outside `data`: key order inside error objects is free.
        let query = "query { a }";
        let exp = json!({"errors": [{
            "message": "boom",
            "extensions": {"code": "x", "path": "$"}
        }]});
        let act = json!({"errors": [{
            "extensions": {"path": "$", "code": "x"},
            "message": "boom"
        }]});
        assert!(response_matches(&exp, &act, Some(query)));
        // ...but error values still have to match.
        let wrong = json!({"errors": [{
            "message": "other",
            "extensions": {"code": "x", "path": "$"}
        }]});
        assert!(!response_matches(&exp, &wrong, Some(query)));
    }

    #[test]
    fn top_level_response_keys_compare_unordered() {
        let query = "query { a }";
        let exp = json!({"data": {"a": 1}, "errors": [{"message": "partial"}]});
        let act = json!({"errors": [{"message": "partial"}], "data": {"a": 1}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn unparsable_query_disables_order_enforcement() {
        assert!(sel_tree_from_query("not a graphql query {{{").is_none());
        let exp = json!({"data": {"a": 1, "b": 2}});
        let act = json!({"data": {"b": 2, "a": 1}});
        assert!(response_matches(&exp, &act, Some("not a graphql query {{{")));
        assert!(response_matches(&exp, &act, None));
    }

    #[test]
    fn sel_tree_covers_operations_and_marks_leaves() {
        let tree = sel_tree_from_query("mutation { insert_x { affected_rows } }").unwrap();
        assert!(tree.contains_key("insert_x"));
        let child = tree.get("insert_x").unwrap().as_ref().unwrap();
        assert!(child.contains_key("affected_rows"));
        // Leaf fields carry no sub-tree.
        assert!(child.get("affected_rows").unwrap().is_none());
    }
}
