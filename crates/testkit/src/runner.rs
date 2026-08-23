//! Discover `*_test.yaml` files under a metadata directory and run them, one
//! stand per test case.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value as Json;

use crate::config::AppTestConfig;
use crate::fixture::load_fixture;
use crate::matching::{response_matches, strip_mcp_content, subset_matches};
use std::collections::BTreeMap;

use crate::model::{
    Actor, Await, Calls, GraphqlStep, ProviderAnswer, SqlStep, Step, TestCase, TestFile, substitute,
};
use crate::provider_stub::ScriptedResponse;
use crate::stand::{Stand, StandConfig};

pub const TEST_FILE_SUFFIX: &str = "_test.yaml";

/// The machine's side of a run; the application's side is `AppTestConfig`.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub engine_binary: PathBuf,
    pub engine_migrations_dir: PathBuf,
    pub admin_database_url: String,
    pub log_dir: PathBuf,
    /// Run only test cases whose `<file>::<name>` contains this.
    pub filter: Option<String>,
}

#[derive(Debug)]
pub struct CaseReport {
    pub file: PathBuf,
    pub name: String,
    pub elapsed: Duration,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub enum Outcome {
    Ok,
    Failed(String),
}

#[derive(Debug, Default)]
pub struct Report {
    pub cases: Vec<CaseReport>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Failed(_)))
            .count()
    }

    pub fn passed(&self) -> usize {
        self.cases.len() - self.failed()
    }

    /// One line per case, then a summary, in the shape `cargo test` prints.
    pub fn write(&self, out: &mut impl std::io::Write, relative_to: &Path) -> std::io::Result<()> {
        for case in &self.cases {
            let file = case
                .file
                .strip_prefix(relative_to)
                .unwrap_or(&case.file)
                .display();
            match &case.outcome {
                Outcome::Ok => writeln!(
                    out,
                    "test {file}::{} ... ok ({:.1}s)",
                    case.name,
                    case.elapsed.as_secs_f64()
                )?,
                Outcome::Failed(reason) => {
                    writeln!(out, "test {file}::{} ... FAILED ({reason})", case.name)?
                }
            }
        }
        writeln!(
            out,
            "\ntest result: {}. {} passed; {} failed",
            if self.failed() == 0 { "ok" } else { "FAILED" },
            self.passed(),
            self.failed()
        )
    }
}

/// Every `*_test.yaml` under `metadata_dir`, sorted.
pub fn discover(metadata_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    walk(metadata_dir, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, found)?;
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(TEST_FILE_SUFFIX))
        {
            found.push(path);
        }
    }
    Ok(())
}

pub fn load_test_file(path: &Path) -> Result<TestFile> {
    let json = load_fixture(path)?;
    serde_json::from_value(json).with_context(|| format!("parsing {}", path.display()))
}

/// Run every test file under the application's metadata directory. Files
/// run in parallel, as many at a time as there are cores, the way cargo runs
/// test binaries; the report keeps the files in discovery order.
pub fn run_all(app: &AppTestConfig, run: &RunConfig) -> Result<Report> {
    let files = discover(&app.metadata)?;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(files.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(file) = files.get(index) else { break };
                    let result = run_file(app, run, file);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((index, result));
                }
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|(index, _)| *index);
    let mut report = Report::default();
    for (index, result) in results {
        match result {
            Ok(file_report) => report.cases.extend(file_report.cases),
            // A file that does not load fails as one case of its own; the
            // other files' outcomes are still reported.
            Err(error) => report.cases.push(CaseReport {
                file: files[index].clone(),
                name: "(file)".to_string(),
                elapsed: Duration::ZERO,
                outcome: Outcome::Failed(format!("{error:#}")),
            }),
        }
    }
    Ok(report)
}

/// Run one test file. A file that does not parse is a failure of every case
/// it would have held — there is no silent skip.
pub fn run_file(app: &AppTestConfig, run: &RunConfig, file: &Path) -> Result<Report> {
    let parsed = load_test_file(file)?;
    let mut report = Report::default();
    for case in &parsed.tests {
        let label = format!("{}::{}", file.display(), case.name);
        if let Some(filter) = &run.filter
            && !label.contains(filter.as_str())
        {
            continue;
        }
        let started = Instant::now();
        let outcome = match run_case(app, run, file, case) {
            Ok(()) => Outcome::Ok,
            Err(error) => Outcome::Failed(format!("{error:#}")),
        };
        report.cases.push(CaseReport {
            file: file.to_path_buf(),
            name: case.name.clone(),
            elapsed: started.elapsed(),
            outcome,
        });
    }
    Ok(report)
}

fn run_case(app: &AppTestConfig, run: &RunConfig, file: &Path, case: &TestCase) -> Result<()> {
    let stem = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("test")
        .trim_end_matches(TEST_FILE_SUFFIX);
    let stand = Stand::boot(&StandConfig {
        name: stem.to_string(),
        engine_binary: run.engine_binary.clone(),
        engine_migrations_dir: run.engine_migrations_dir.clone(),
        app_migrations_dir: app.migrations.clone(),
        metadata_dir: app.metadata.clone(),
        source: app.source.clone(),
        admin_database_url: run.admin_database_url.clone(),
        log_dir: run.log_dir.clone(),
        env: app
            .engine_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    })
    .context("booting the stand")?;
    let mut cx = CaseContext {
        stand,
        source: app.source.clone(),
        actor: None,
        vars: BTreeMap::new(),
    };
    for (index, raw) in case.steps.iter().enumerate() {
        let kind = Step::kind_of(raw);
        cx.run_step(raw).with_context(|| {
            format!(
                "step {} {kind}; engine log: {}",
                index + 1,
                cx.stand.log_path().display()
            )
        })?;
    }
    Ok(())
}

const AWAIT_DEADLINE: Duration = Duration::from_secs(60);
const AWAIT_POLL: Duration = Duration::from_millis(50);

struct CaseContext {
    stand: Stand,
    source: String,
    actor: Option<Actor>,
    vars: BTreeMap<String, Json>,
}

impl CaseContext {
    fn run_step(&mut self, raw: &Json) -> Result<()> {
        let step = Step::parse(substitute(raw, &self.vars)?)?;
        match step {
            Step::Http(conf) => self.http(&conf),
            Step::Sql(sql) => self.sql(&sql),
            Step::As(step) => {
                self.actor = Some(step.actor);
                Ok(())
            }
            Step::Graphql(step) => self.graphql(&step),
            Step::Providers(answers) => {
                self.providers(answers);
                Ok(())
            }
            Step::Await(step) => match &step.what {
                Await::Terminal {
                    terminal,
                    expect,
                    capture,
                } => self.await_terminal(terminal, expect.as_ref(), capture),
                Await::Row { row, capture } => self.await_row(row, capture),
                Await::Receptive { receptive, state } => self.await_receptive(receptive, state),
                Await::Held { held } => {
                    if self.stand.providers().await_held(held, AWAIT_DEADLINE) {
                        Ok(())
                    } else {
                        let mut client = self.stand.pg()?;
                        Err(anyhow!(
                            "no request to {held} was held: {}",
                            process_diagnostics(&mut client, &self.source)
                        ))
                    }
                }
            },
            Step::Hold(path) => {
                self.stand.providers().hold(&path);
                Ok(())
            }
            Step::Release(path) => {
                self.stand.providers().release(&path);
                Ok(())
            }
            Step::Calls(step) => self.calls(&step.calls),
        }
    }

    fn capture(
        &mut self,
        names: &BTreeMap<String, String>,
        value: &Json,
        what: &str,
    ) -> Result<()> {
        for (name, pointer) in names {
            let found = value.pointer(pointer).ok_or_else(|| {
                anyhow!(
                    "capture `{name}`: no value at {pointer} in {what}:\n{}",
                    pretty(value)
                )
            })?;
            self.vars.insert(name.clone(), found.clone());
        }
        Ok(())
    }

    fn sql(&mut self, step: &SqlStep) -> Result<()> {
        let mut client = self.stand.pg()?;
        if step.expect.is_some() && step.error.is_some() {
            return Err(anyhow!(
                "a sql step has either `expect` or `error`, not both"
            ));
        }
        if let Some(class) = step.error {
            return match client.batch_execute(&step.sql) {
                Ok(()) => Err(anyhow!(
                    "expected {class:?}, but the statement succeeded:\n{}",
                    step.sql
                )),
                Err(error) => {
                    let got = error.code().map(|c| c.code().to_string());
                    if got.as_deref() == Some(class.sqlstate()) {
                        Ok(())
                    } else {
                        Err(anyhow!(
                            "expected {class:?} (SQLSTATE {}), got {}: {}",
                            class.sqlstate(),
                            got.as_deref().unwrap_or("no SQLSTATE"),
                            describe_pg_error(&error)
                        ))
                    }
                }
            };
        }
        if step.expect.is_none() && step.capture.is_empty() {
            client
                .batch_execute(&step.sql)
                .with_context(|| format!("executing:\n{}", step.sql))?;
            return Ok(());
        }
        // Postgres renders each row as JSON, so a test compares values the
        // way the API would show them rather than through a type mapping here.
        // A CTE, not a subquery: `INSERT ... RETURNING` is a valid statement
        // to capture from, and only a CTE admits it.
        let wrapped = format!(
            "WITH donat_test_row AS ({}) SELECT to_jsonb(donat_test_row) FROM donat_test_row",
            step.sql.trim().trim_end_matches(';')
        );
        let rows = client
            .query(&wrapped, &[])
            .with_context(|| format!("executing:\n{}", step.sql))?
            .into_iter()
            .map(|row| row.get::<_, Json>(0))
            .collect::<Vec<_>>();
        let actual = Json::Array(rows);
        if let Some(expected) = &step.expect
            && !subset_matches(&Json::Array(expected.clone()), &actual)
        {
            return Err(anyhow!(
                "rows mismatch for:\n{}\nexpected:\n{}\nactual:\n{}",
                step.sql,
                pretty(&Json::Array(expected.clone())),
                pretty(&actual)
            ));
        }
        if !step.capture.is_empty() {
            let first = actual
                .get(0)
                .ok_or_else(|| {
                    anyhow!("capture from a query that returned no rows:\n{}", step.sql)
                })?
                .clone();
            let pointers = step
                .capture
                .iter()
                .map(|(name, column)| (name.clone(), format!("/{column}")))
                .collect();
            self.capture(&pointers, &first, "the first row")?;
        }
        Ok(())
    }

    fn actor_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(actor) = &self.actor {
            headers.push(("X-Donat-Role".to_string(), actor.role.clone()));
            if let Some(user) = &actor.user {
                headers.push(("X-Donat-User-Id".to_string(), user.clone()));
            }
        }
        headers
    }

    fn graphql(&mut self, step: &GraphqlStep) -> Result<()> {
        let mut body = serde_json::Map::new();
        body.insert("query".into(), Json::String(step.graphql.clone()));
        if let Some(variables) = &step.variables {
            body.insert("variables".into(), variables.clone());
        }
        let body = Json::Object(body);
        let (code, resp) =
            self.stand
                .request("POST", "/v1/graphql", &self.actor_headers(), Some(&body))?;
        if code != 200 {
            return Err(anyhow!(
                "status {code} for:\n{}\nresponse:\n{}",
                step.graphql,
                pretty(&resp)
            ));
        }
        match &step.expect {
            Some(expect) => {
                if !subset_matches(expect, &resp) {
                    return Err(anyhow!(
                        "response mismatch for:\n{}\nexpected:\n{}\nactual:\n{}",
                        step.graphql,
                        pretty(expect),
                        pretty(&resp)
                    ));
                }
            }
            None => {
                if resp.get("errors").is_some() {
                    return Err(anyhow!(
                        "errors for:\n{}\nresponse:\n{}",
                        step.graphql,
                        pretty(&resp)
                    ));
                }
            }
        }
        self.capture(&step.capture, &resp, "the response")
    }

    fn providers(&mut self, answers: BTreeMap<String, ProviderAnswer>) {
        let stub = self.stand.providers();
        for (path, answer) in answers {
            match answer {
                ProviderAnswer::Default(body) => {
                    stub.set_default(&path, ScriptedResponse::ok(body))
                }
                ProviderAnswer::Queue(queue) => stub.script(
                    &path,
                    queue
                        .into_iter()
                        .map(|a| ScriptedResponse {
                            status: a.status,
                            body: a.body,
                        })
                        .collect(),
                ),
            }
        }
    }

    fn await_row(&mut self, table: &str, capture: &BTreeMap<String, String>) -> Result<()> {
        let mut client = self.stand.pg()?;
        let deadline = Instant::now() + AWAIT_DEADLINE;
        let sql = format!("SELECT to_jsonb(donat_test_row) FROM {table} donat_test_row LIMIT 1");
        let row = loop {
            if let Some(row) = client
                .query_opt(&sql, &[])
                .with_context(|| format!("polling {table}"))?
            {
                break row.get::<_, Json>(0);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "{table} never received a row: {}",
                    process_diagnostics(&mut client, &self.source)
                ));
            }
            std::thread::sleep(AWAIT_POLL);
        };
        let pointers = capture
            .iter()
            .map(|(name, column)| (name.clone(), format!("/{column}")))
            .collect();
        self.capture(&pointers, &row, &format!("the first {table} row"))
    }

    fn await_receptive(&mut self, process: &str, state: &str) -> Result<()> {
        let mut client = self.stand.pg()?;
        let deadline = Instant::now() + AWAIT_DEADLINE;
        loop {
            let receptive: bool = client
                .query_one(
                    "SELECT EXISTS (
                         SELECT 1
                         FROM donat.process_events event
                         JOIN donat.process_instances instance
                           ON instance.source_name = event.source_name
                          AND instance.id = event.instance_id
                         WHERE event.source_name = $1
                           AND event.kind = 'timer'
                           AND event.status = 'pending'
                           AND event.payload_json ->> 'wait_state' = $3
                           AND instance.process_name = $2
                           AND instance.current_state = $3
                     )",
                    &[&self.source, &process, &state],
                )
                .context("polling donat.process_events")?
                .get(0);
            if receptive {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "process '{process}' never became receptive in '{state}': {}",
                    process_diagnostics(&mut client, &self.source)
                ));
            }
            std::thread::sleep(AWAIT_POLL);
        }
    }

    fn await_terminal(
        &mut self,
        process: &str,
        expect: Option<&Json>,
        capture: &BTreeMap<String, String>,
    ) -> Result<()> {
        let mut client = self.stand.pg()?;
        let deadline = Instant::now() + AWAIT_DEADLINE;
        let output = loop {
            let row = client
                .query_opt(
                    "SELECT status, terminal_output_json
                     FROM donat.process_instances
                     WHERE source_name = $1 AND process_name = $2",
                    &[&self.source, &process],
                )
                .context("polling donat.process_instances")?;
            if let Some(row) = row {
                let status: String = row.get(0);
                if status != "running" {
                    if status != "terminal" {
                        return Err(anyhow!(
                            "process '{process}' ended with status {status}: {}",
                            process_diagnostics(&mut client, &self.source)
                        ));
                    }
                    break row.get::<_, Option<Json>>(1).unwrap_or(Json::Null);
                }
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "process '{process}' never reached a terminal state: {}",
                    process_diagnostics(&mut client, &self.source)
                ));
            }
            std::thread::sleep(AWAIT_POLL);
        };
        if let Some(expect) = expect
            && !subset_matches(expect, &output)
        {
            return Err(anyhow!(
                "terminal output of '{process}' mismatch\nexpected:\n{}\nactual:\n{}",
                pretty(expect),
                pretty(&output)
            ));
        }
        self.capture(capture, &output, "the terminal output")
    }

    fn calls(&mut self, calls: &Calls) -> Result<()> {
        let recorded = self.stand.providers().calls_for(&calls.path);
        if let Some(count) = calls.count
            && recorded.len() != count
        {
            return Err(anyhow!(
                "{} call(s) to {}, expected {count}",
                recorded.len(),
                calls.path
            ));
        }
        if calls.body.is_none() && calls.headers.is_empty() {
            return Ok(());
        }
        let call = recorded.get(calls.index).ok_or_else(|| {
            anyhow!(
                "no call #{} to {} ({} recorded)",
                calls.index,
                calls.path,
                recorded.len()
            )
        })?;
        if let Some(body) = &calls.body
            && !subset_matches(body, &call.body)
        {
            return Err(anyhow!(
                "call #{} to {}: body mismatch\nexpected:\n{}\nactual:\n{}",
                calls.index,
                calls.path,
                pretty(body),
                pretty(&call.body)
            ));
        }
        for (name, value) in &calls.headers {
            let got = call.header(name);
            if got != Some(value.as_str()) {
                return Err(anyhow!(
                    "call #{} to {}: header {name} is {:?}, expected {value:?}",
                    calls.index,
                    calls.path,
                    got
                ));
            }
        }
        Ok(())
    }

    /// The conformance `http_case`, returning instead of panicking.
    fn http(&mut self, conf: &Json) -> Result<()> {
        let url = conf
            .get("url")
            .and_then(Json::as_str)
            .ok_or_else(|| anyhow!("`url` must be a string"))?;
        let headers = conf_headers(conf);
        let exp_status = conf.get("status").and_then(Json::as_u64).unwrap_or(200) as u16;
        let method = conf.get("method").and_then(Json::as_str).unwrap_or("POST");
        let body = match method {
            "GET" => None,
            "POST" => Some(
                conf.get("query")
                    .or_else(|| conf.get("body"))
                    .cloned()
                    .unwrap_or(Json::Null),
            ),
            _ => conf.get("body").cloned(),
        };
        let mut headers = headers;
        if url == "/mcp" {
            let has_accept = headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("accept"));
            let has_protocol = headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("MCP-Protocol-Version"));
            if !has_accept {
                headers.push((
                    "Accept".into(),
                    "application/json, text/event-stream".into(),
                ));
            }
            let initialize = body
                .as_ref()
                .and_then(|b| b.get("method"))
                .and_then(Json::as_str)
                == Some("initialize");
            if !has_protocol && !initialize {
                headers.push(("MCP-Protocol-Version".into(), "2025-06-18".into()));
            }
        }
        let (code, resp) = self.stand.request(method, url, &headers, body.as_ref())?;
        if code != exp_status {
            return Err(anyhow!(
                "{method} {url}: status {code}, expected {exp_status}\nresponse:\n{}",
                pretty(&resp)
            ));
        }
        let resp = if url == "/mcp" {
            strip_mcp_content(&resp)
        } else {
            resp
        };
        let query_text = conf
            .get("query")
            .and_then(|q| q.get("query"))
            .and_then(Json::as_str);
        let normalize = |exp: &Json| {
            if url == "/mcp" {
                strip_mcp_content(exp)
            } else {
                exp.clone()
            }
        };
        if let Some(allowed) = conf.get("allowed_responses").and_then(Json::as_array) {
            let ok = allowed.iter().any(|a| {
                a.get("response")
                    .map(normalize)
                    .is_some_and(|exp| response_matches(&exp, &resp, query_text))
            });
            if !ok {
                return Err(anyhow!(
                    "{method} {url}: response matched none of allowed_responses\nactual:\n{}",
                    pretty(&resp)
                ));
            }
        } else if let Some(exp) = conf.get("response") {
            let exp = normalize(exp);
            if !response_matches(&exp, &resp, query_text) {
                return Err(anyhow!(
                    "{method} {url}: response mismatch\nexpected:\n{}\nactual:\n{}",
                    pretty(&exp),
                    pretty(&resp)
                ));
            }
        }
        Ok(())
    }
}

/// `headers:` or the singular `header:` some upstream fixtures spell.
fn conf_headers(conf: &Json) -> Vec<(String, String)> {
    conf.get("headers")
        .or_else(|| conf.get("header"))
        .and_then(Json::as_object)
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

/// Durable evidence for a process that did not finish the way a test
/// expects: instances, activity jobs, signals, fan-out items, stuck events.
fn process_diagnostics(client: &mut postgres::Client, source: &str) -> String {
    let mut report = String::new();
    let mut section = |label: &str, sql: &str| {
        if let Ok(rows) = client.query(sql, &[&source]) {
            for row in rows {
                report.push_str(label);
                for (i, column) in row.columns().iter().enumerate() {
                    let value: Option<String> = row.try_get(i).ok().flatten();
                    report.push_str(&format!(
                        " {}={}",
                        column.name(),
                        value.unwrap_or_else(|| "null".into())
                    ));
                }
                report.push_str("; ");
            }
        }
    };
    section(
        "instance",
        "SELECT process_name::text, status::text, current_state::text,
                terminal_output_json::text AS output
         FROM donat.process_instances WHERE source_name = $1 ORDER BY created_at",
    );
    section(
        "job",
        "SELECT status::text, attempts::text, last_error_json::text AS error
         FROM donat.process_activity_jobs WHERE source_name = $1 ORDER BY id",
    );
    section(
        "signal",
        "SELECT process_name::text, signal_name::text, status::text,
                correlation_json::text AS correlate
         FROM donat.process_signal_requests WHERE source_name = $1 ORDER BY id",
    );
    section(
        "fanout",
        "SELECT state_name::text, ordinal::text, status::text, failure_json::text AS failure
         FROM donat.process_fanout_items WHERE source_name = $1 ORDER BY state_name, ordinal",
    );
    section(
        "event",
        "SELECT kind::text, status::text, attempts::text,
                left(payload_json::text, 400) AS payload
         FROM donat.process_events WHERE source_name = $1 AND status <> 'consumed' ORDER BY id",
    );
    if report.is_empty() {
        "no durable process state recorded".into()
    } else {
        report
    }
}

/// `postgres::Error` displays as "db error"; the message is in the payload.
fn describe_pg_error(error: &postgres::Error) -> String {
    match error.as_db_error() {
        Some(db) => match db.detail() {
            Some(detail) => format!("{} ({detail})", db.message()),
            None => db.message().to_string(),
        },
        None => error.to_string(),
    }
}

fn pretty(v: &Json) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
