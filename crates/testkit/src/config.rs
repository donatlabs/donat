//! `donat.test.yaml` — the application's side of a stand.
//!
//! It sits at the application root beside `metadata/` and `migrations/` and
//! says what is true of the application on every machine: where its metadata
//! and migrations are, and which environment the engine needs, with
//! `${providers}` standing for the runner's provider stub. Where Postgres is
//! and which binary runs are properties of the machine and come from the
//! command line or the environment instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const FILE_NAME: &str = "donat.test.yaml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTestConfig {
    /// Metadata directory, relative to the config file.
    #[serde(default = "default_metadata")]
    pub metadata: PathBuf,
    /// Application migrations directory, relative to the config file; absent
    /// means the application has none.
    #[serde(default)]
    pub migrations: Option<PathBuf>,
    /// The source whose Process revisions `migrate` deploys.
    #[serde(default = "default_source")]
    pub source: String,
    /// Environment for `migrate` and `serve`. `${providers}` in a value is
    /// the provider stub's base URL.
    #[serde(default)]
    pub engine_env: BTreeMap<String, String>,
}

fn default_metadata() -> PathBuf {
    PathBuf::from("metadata")
}

fn default_source() -> String {
    "default".to_string()
}

impl AppTestConfig {
    /// Read `<app_dir>/donat.test.yaml`, resolving its paths against `app_dir`.
    pub fn load(app_dir: &Path) -> Result<Self> {
        let path = app_dir.join(FILE_NAME);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Self =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.metadata = app_dir.join(&cfg.metadata);
        cfg.migrations = cfg.migrations.as_ref().map(|m| app_dir.join(m));
        Ok(cfg)
    }
}
