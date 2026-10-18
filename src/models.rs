//! Discovers which models (and reasoning efforts per model) each agent CLI
//! actually offers, so the UI only presents valid choices.
//!
//! - Claude Code: the CLI's own `initialize` control request in stream-json
//!   mode (no model call is made).
//! - Codex: `codex app-server`'s `model/list`.
//! - Hermes: its provider model cache for the configured provider; efforts for
//!   `openai-codex` models come from Codex's list.

use crate::{config::AgentConfig, runs::ModelInfo};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Hermes' Anthropic provider calls the API with Claude Code's subscription
/// login instead of the `claude` CLI; feedback-hub never routes runs there.
pub fn hermes_model_forbidden(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("claude") || model.starts_with("anthropic") || model.contains("fable")
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Writes `requests` to a JSON-lines process and returns the first message
/// matching `done`, giving up after `timeout`.
fn json_exchange(
    mut command: Command,
    requests: &[Value],
    timeout: Duration,
    done: impl Fn(&Value) -> bool,
) -> Result<Value> {
    let mut child: Child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting the agent CLI")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    for request in requests {
        writeln!(stdin, "{request}")?;
    }
    let stdout = child.stdout.take().context("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let result = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && done(&value)
                {
                    break Ok(value);
                }
            }
            Err(_) => break Err(anyhow::anyhow!("no answer from the CLI")),
        }
    };
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Default model label (e.g. "Opus 5.5") plus the selectable models.
pub struct Discovered {
    pub default_model: Option<String>,
    pub default_efforts: Vec<String>,
    pub models: Vec<ModelInfo>,
}

pub fn claude(program: &str, agent: &AgentConfig) -> Result<Discovered> {
    let mut command = Command::new(program);
    agent.apply_env("claude", &mut command)?;
    command.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ]);
    let response = json_exchange(
        command,
        &[
            json!({"type": "control_request", "request_id": "models", "request": {"subtype": "initialize"}}),
        ],
        Duration::from_secs(30),
        |v| str_field(v, "type") == Some("control_response"),
    )?;
    let body = response
        .pointer("/response/response")
        .or_else(|| response.get("response"))
        .unwrap_or(&Value::Null);
    let entries = body
        .get("models")
        .and_then(Value::as_array)
        .context("Claude Code did not list models")?;
    let efforts = |m: &Value| -> Vec<String> {
        if m.get("supportsEffort").and_then(Value::as_bool) != Some(true) {
            return vec![];
        }
        m.get("supportedEffortLevels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(String::from))
            .collect()
    };
    let mut discovered = Discovered {
        default_model: None,
        default_efforts: vec![],
        models: vec![],
    };
    for m in entries {
        let Some(id) = str_field(m, "value") else {
            continue;
        };
        if id == "default" {
            discovered.default_model = str_field(m, "description")
                .and_then(|d| d.split(" · ").next())
                .map(String::from)
                .or_else(|| str_field(m, "resolvedModel").map(String::from));
            discovered.default_efforts = efforts(m);
            continue;
        }
        discovered.models.push(ModelInfo {
            id: id.to_string(),
            name: str_field(m, "displayName").unwrap_or(id).to_string(),
            description: str_field(m, "description").map(String::from),
            efforts: efforts(m),
            default_effort: None,
        });
    }
    Ok(discovered)
}

pub fn codex(program: &str) -> Result<Discovered> {
    let mut command = Command::new(program);
    command.arg("app-server");
    let response = json_exchange(
        command,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "feedback-hub", "version": env!("CARGO_PKG_VERSION")}}}),
            json!({"jsonrpc": "2.0", "method": "initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "model/list", "params": {}}),
        ],
        Duration::from_secs(30),
        |v| v.get("id").and_then(Value::as_i64) == Some(2),
    )?;
    if let Some(error) = response.get("error") {
        bail!("codex model/list failed: {error}");
    }
    let entries = response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .context("Codex did not list models")?;
    let mut discovered = Discovered {
        default_model: None,
        default_efforts: vec![],
        models: vec![],
    };
    for m in entries {
        if m.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = str_field(m, "model").or(str_field(m, "id")) else {
            continue;
        };
        let info = ModelInfo {
            id: id.to_string(),
            name: str_field(m, "displayName").unwrap_or(id).to_string(),
            description: str_field(m, "description").map(String::from),
            efforts: m
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| str_field(e, "reasoningEffort").map(String::from))
                .collect(),
            default_effort: str_field(m, "defaultReasoningEffort").map(String::from),
        };
        if m.get("isDefault").and_then(Value::as_bool) == Some(true) {
            discovered.default_model = Some(info.name.clone());
            discovered.default_efforts = info.efforts.clone();
        }
        discovered.models.push(info);
    }
    Ok(discovered)
}

/// Reads `model.provider` / `model.default` from Hermes' config.yaml.
fn hermes_config(home: &std::path::Path) -> (Option<String>, Option<String>) {
    let text = std::fs::read_to_string(home.join(".hermes/config.yaml")).unwrap_or_default();
    let (mut provider, mut default) = (None, None);
    let mut in_model = false;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_model = line.trim_end() == "model:";
            continue;
        }
        if in_model {
            let trimmed = line.trim();
            let value = |key: &str| {
                trimmed
                    .strip_prefix(key)
                    .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            };
            if let Some(v) = value("provider:") {
                provider = Some(v);
            } else if let Some(v) = value("default:") {
                default = Some(v);
            }
        }
    }
    (provider, default)
}

pub fn hermes(home: &std::path::Path, codex_models: &[ModelInfo]) -> Result<Discovered> {
    let (provider, default) = hermes_config(home);
    let provider = provider.context("no model.provider in ~/.hermes/config.yaml")?;
    if provider == "anthropic" {
        bail!(
            "Hermes is configured for the anthropic provider, which uses the Claude subscription login outside the claude CLI; feedback-hub won't run it"
        );
    }
    let cache: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".hermes/provider_models_cache.json"))
            .context("reading Hermes' model cache (run `hermes model --refresh`)")?,
    )?;
    let ids: Vec<String> = cache
        .pointer(&format!("/{provider}/models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(String::from))
        .filter(|m| !hermes_model_forbidden(m))
        .collect();
    let generic: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
        .map(String::from)
        .to_vec();
    let name_for = |id: &str| -> String {
        let base = id.trim_end_matches("-900k");
        let name = codex_models
            .iter()
            .find(|m| m.id == base)
            .map_or(base, |m| m.name.as_str());
        if id.ends_with("-900k") {
            format!("{name} (900k context)")
        } else {
            name.to_string()
        }
    };
    let efforts_for = |id: &str| -> (Vec<String>, Option<String>) {
        if provider == "openai-codex" {
            let base = id.trim_end_matches("-900k");
            if let Some(m) = codex_models.iter().find(|m| m.id == base) {
                return (m.efforts.clone(), m.default_effort.clone());
            }
        }
        (generic.clone(), None)
    };
    let models: Vec<ModelInfo> = ids
        .iter()
        .map(|id| {
            let (efforts, default_effort) = efforts_for(id);
            ModelInfo {
                id: id.clone(),
                name: name_for(id),
                description: Some(format!("via {provider}")),
                efforts,
                default_effort,
            }
        })
        .collect();
    let default_efforts = default
        .as_deref()
        .map(|d| efforts_for(d).0)
        .unwrap_or(generic);
    Ok(Discovered {
        default_model: default.map(|d| format!("{d} via {provider}")),
        default_efforts,
        models,
    })
}

/// Models from the worker config (`models = [...]`), with the agent's efforts.
pub fn configured(agent: &AgentConfig, kind: &str) -> Option<Discovered> {
    let models = agent.models.as_ref()?;
    let efforts = agent.efforts(kind);
    Some(Discovered {
        default_model: None,
        default_efforts: efforts.clone(),
        models: models
            .iter()
            .map(|id| ModelInfo {
                id: id.clone(),
                name: id.clone(),
                description: None,
                efforts: efforts.clone(),
                default_effort: None,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hermes_provider_and_models() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".hermes")).unwrap();
        std::fs::write(
            home.path().join(".hermes/config.yaml"),
            "model:\n  default: gpt-6-luna\n  provider: openai-codex\n  reasoning_effort: xhigh\nother:\n  provider: x\n",
        )
        .unwrap();
        std::fs::write(
            home.path().join(".hermes/provider_models_cache.json"),
            r#"{"openai-codex": {"models": ["gpt-6-luna", "gpt-6-luna-900k", "claude-oops"]}}"#,
        )
        .unwrap();
        let codex = vec![ModelInfo {
            id: "gpt-6-luna".into(),
            name: "GPT-6-Luna".into(),
            description: None,
            efforts: vec!["low".into(), "max".into()],
            default_effort: Some("low".into()),
        }];
        let found = hermes(home.path(), &codex).unwrap();
        let ids: Vec<&str> = found.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gpt-6-luna", "gpt-6-luna-900k"]);
        assert_eq!(found.models[1].efforts, ["low", "max"]);
        assert_eq!(
            found.default_model.as_deref(),
            Some("gpt-6-luna via openai-codex")
        );

        std::fs::write(
            home.path().join(".hermes/config.yaml"),
            "model:\n  provider: anthropic\n",
        )
        .unwrap();
        assert!(hermes(home.path(), &codex).is_err());
        assert!(hermes_model_forbidden("anthropic/claude-sonnet-5"));
        assert!(!hermes_model_forbidden("gpt-6-sol"));
    }
}
