//! Client-side configuration for the CLI and the agent worker.
//!
//! Read from `$FEEDBACK_HUB_CONFIG` or `~/.config/feedback-hub/config.toml`;
//! `FEEDBACK_HUB_URL` and `FEEDBACK_HUB_TOKEN` override the file.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct ClientConfig {
    pub url: Option<String>,
    pub token: Option<String>,
    /// Alternative to `token`: a file containing the token.
    pub token_file: Option<String>,
    #[serde(default)]
    pub worker: WorkerConfig,
    /// Extra or overridden agent commands, keyed by agent name.
    #[serde(default)]
    pub agents: BTreeMap<String, AgentConfig>,
    /// App id → local checkout overrides.
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct WorkerConfig {
    pub name: Option<String>,
    pub default_agent: Option<String>,
    /// Agents this worker runs; defaults to every configured agent.
    pub agents: Option<Vec<String>>,
    pub poll_seconds: Option<u64>,
    pub max_runtime_minutes: Option<u64>,
    pub worktree_root: Option<String>,
    pub state_dir: Option<String>,
    /// Also pick up new `open` feedback without waiting for someone to queue it.
    #[serde(default)]
    pub include_open: bool,
    /// `push` (default branch), `branch` (push the feedback branch) or `pr`.
    pub delivery: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// argv; `{prompt}`, `{prompt_file}`, `{workdir}`, `{id}` and `{app}` are substituted.
    pub command: Vec<String>,
}

pub fn default_agents() -> BTreeMap<String, AgentConfig> {
    let agent = |argv: &[&str]| AgentConfig {
        command: argv.iter().map(|s| s.to_string()).collect(),
    };
    BTreeMap::from([
        (
            "claude".to_string(),
            agent(&[
                "claude",
                "-p",
                "--permission-mode",
                "bypassPermissions",
                "{prompt}",
            ]),
        ),
        (
            "codex".to_string(),
            agent(&[
                "codex",
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "{workdir}",
                "{prompt}",
            ]),
        ),
        (
            "hermes".to_string(),
            agent(&[
                "hermes",
                "chat",
                "-Q",
                "--yolo",
                "--in",
                "{workdir}",
                "--query-file",
                "{prompt_file}",
            ]),
        ),
    ])
}

pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FEEDBACK_HUB_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("feedback-hub/config.toml")
}

impl ClientConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut config: ClientConfig = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        if let Ok(url) = std::env::var("FEEDBACK_HUB_URL") {
            config.url = Some(url);
        }
        if let Ok(token) = std::env::var("FEEDBACK_HUB_TOKEN") {
            config.token = Some(token);
        }
        if config.token.is_none()
            && let Some(file) = &config.token_file
        {
            let token = std::fs::read_to_string(expand_home(file))
                .with_context(|| format!("reading token file {file}"))?;
            config.token = Some(token.trim().to_string());
        }
        Ok(config)
    }

    pub fn url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| "https://feedback.fruitiex.org".into())
            .trim_end_matches('/')
            .to_string()
    }

    pub fn token(&self) -> Result<String> {
        match &self.token {
            Some(token) if !token.is_empty() => Ok(token.clone()),
            _ => bail!(
                "no feedback-hub token configured; set FEEDBACK_HUB_TOKEN or `token` in {}",
                config_path().display()
            ),
        }
    }

    pub fn agents(&self) -> BTreeMap<String, AgentConfig> {
        let mut agents = default_agents();
        agents.extend(self.agents.clone());
        agents
    }
}
