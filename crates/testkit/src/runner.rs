//! Discover `*_test.yaml` files under a metadata directory and run them, one
//! stand per test case.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value as Json;

use crate::config::AppTestConfig;
use crate::fixture::load_fixture;
use crate::matching::{response_matches, strip_mcp_content};
use crate::model::{SqlStep, Step, TestCase, TestFile};
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

/// Run every test file under the application's metadata directory.
pub fn run_all(app: &AppTestConfig, run: &RunConfig) -> Result<Report> {
    let mut report = Report::default();
    for file in discover(&app.metadata)? {
        report.cases.extend(run_file(app, run, &file)?.cases);
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
    let mut cx = CaseContext { stand };
    for (index, step) in case.steps.iter().enumerate() {
        cx.run_step(step).with_context(|| {
            format!(
                "step {} {}; engine log: {}",
                index + 1,
                step.kind(),
                cx.stand.log_path().display()
            )
        })?;
    }
    Ok(())
}

struct CaseContext {
    stand: Stand,
}

impl CaseContext {
    fn run_step(&mut self, step: &Step) -> Result<()> {
        match step {
            Step::Http(conf) => self.http(conf),
            Step::Sql(sql) => self.sql(sql),
        }
    }

    fn sql(&mut self, step: &SqlStep) -> Result<()> {
        let mut client = self.stand.pg()?;
        client
            .batch_execute(&step.sql)
            .with_context(|| format!("executing:\n{}", step.sql))?;
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

fn pretty(v: &Json) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
