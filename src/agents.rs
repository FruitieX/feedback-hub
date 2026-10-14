//! Adapters for the coding-agent CLIs: how to start or resume a session with a
//! given model and reasoning effort, and how to turn their streamed JSON output
//! into run events.
//!
//! Only the official CLIs are executed (for Claude Code that is the `claude`
//! binary in print mode with the user's own login); nothing here talks to a
//! model API directly.

use crate::config::AgentConfig;
use serde_json::Value;
use std::path::Path;

/// What to feed the process on stdin.
pub enum Stdin {
    None,
    /// Plain text, then close stdin.
    Text(String),
    /// Claude's stream-json input: the first user message; stdin stays open for steering.
    Claude(String),
}

pub struct Invocation {
    pub argv: Vec<String>,
    pub stdin: Stdin,
    /// Messages can be written to the running process (Claude Code).
    pub live: bool,
}

pub struct TurnInput<'a> {
    /// The full task prompt for a new session, or the follow-up message for a resumed one.
    pub text: &'a str,
    /// File holding `text`.
    pub text_file: &'a Path,
    pub workdir: &'a Path,
    pub resume: Option<&'a str>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub feedback_id: i64,
    pub app: &'a str,
}

pub fn claude_user_message(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    })
    .to_string()
}

pub fn invocation(kind: &str, agent: &AgentConfig, input: &TurnInput) -> Invocation {
    let program = agent.program.clone().unwrap_or_else(|| kind.to_string());
    let mut argv: Vec<String> = vec![program];
    let mut push = |args: &[&str]| argv.extend(args.iter().map(|s| s.to_string()));
    let (stdin, live) = match kind {
        "claude" => {
            push(&[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
            ]);
            if let Some(model) = input.model {
                push(&["--model", model]);
            }
            if let Some(effort) = input.effort {
                push(&["--effort", effort]);
            }
            if let Some(session) = input.resume {
                push(&["--resume", session]);
            }
            (Stdin::Claude(input.text.to_string()), true)
        }
        "codex" => {
            push(&["exec"]);
            if input.resume.is_some() {
                push(&["resume"]);
            }
            push(&[
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
            ]);
            if input.resume.is_none() {
                push(&["-C", &input.workdir.to_string_lossy()]);
            }
            if let Some(model) = input.model {
                push(&["-m", model]);
            }
            if let Some(effort) = input.effort {
                push(&["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
            }
            if let Some(session) = input.resume {
                push(&[session]);
            }
            push(&["-"]);
            (Stdin::Text(input.text.to_string()), false)
        }
        "hermes" => {
            push(&[
                "chat",
                "--format",
                "stream-json",
                "--yolo",
                "--in",
                &input.workdir.to_string_lossy(),
                "--query-file",
                &input.text_file.to_string_lossy(),
            ]);
            if let Some(model) = input.model {
                push(&["-m", model]);
            }
            if let Some(effort) = input.effort {
                push(&["--reasoning", effort]);
            }
            if let Some(session) = input.resume {
                push(&["--resume", session]);
            }
            (Stdin::None, false)
        }
        _ => {
            argv = agent
                .command
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|arg| {
                    arg.replace("{prompt_file}", &input.text_file.to_string_lossy())
                        .replace("{workdir}", &input.workdir.to_string_lossy())
                        .replace("{id}", &input.feedback_id.to_string())
                        .replace("{app}", input.app)
                        .replace("{model}", input.model.unwrap_or(""))
                        .replace("{effort}", input.effort.unwrap_or(""))
                        .replace("{prompt}", input.text)
                })
                .collect();
            return Invocation {
                argv,
                stdin: Stdin::None,
                live: false,
            };
        }
    };
    argv.extend(agent.args.iter().cloned());
    Invocation { argv, stdin, live }
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Event(&'static str, String),
    Session(String),
    /// The agent finished a turn; `text` is its final answer.
    Result {
        text: Option<String>,
        is_error: bool,
        cost_usd: Option<f64>,
        tokens: Option<i64>,
    },
}

const TOOL_OUTPUT_CHARS: usize = 2000;

fn clip(text: &str, max: usize) -> String {
    let text = text.trim_end();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// One line per tool call, e.g. `Bash: cargo test` or `Edit: src/main.rs`.
fn describe_tool(name: &str, input: &Value) -> String {
    let main = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| str_field(input, k));
    match main {
        Some(text) => format!("{name}: {}", clip(text, 500)),
        None if input.is_null() => name.to_string(),
        None => format!("{name} {}", clip(&input.to_string(), 300)),
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| str_field(p, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Turns an agent's stdout lines into run events.
pub struct Parser {
    kind: String,
    /// Hermes streams text in deltas; they are joined until the next non-text event.
    text: String,
}

impl Parser {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            text: String::new(),
        }
    }

    pub fn line(&mut self, line: &str) -> Vec<Parsed> {
        let line = line.trim_end();
        if line.is_empty() {
            return vec![];
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![Parsed::Event("output", clip(line, TOOL_OUTPUT_CHARS))];
        };
        match self.kind.as_str() {
            "claude" => claude(&value),
            "codex" => codex(&value),
            "hermes" => self.hermes(&value),
            _ => vec![Parsed::Event("output", clip(line, TOOL_OUTPUT_CHARS))],
        }
    }

    pub fn finish(&mut self) -> Vec<Parsed> {
        self.flush_text().into_iter().collect()
    }

    fn flush_text(&mut self) -> Option<Parsed> {
        let text = std::mem::take(&mut self.text);
        (!text.trim().is_empty()).then(|| Parsed::Event("text", text.trim().to_string()))
    }

    fn hermes(&mut self, v: &Value) -> Vec<Parsed> {
        let kind = str_field(v, "type").unwrap_or("");
        if kind == "text" {
            self.text.push_str(str_field(v, "text").unwrap_or(""));
            return vec![];
        }
        let mut out: Vec<Parsed> = self.flush_text().into_iter().collect();
        match kind {
            "system" => {
                if let Some(session) = str_field(v, "session_id").filter(|s| !s.is_empty()) {
                    out.push(Parsed::Session(session.into()));
                }
                if let Some(model) = str_field(v, "model").filter(|s| !s.is_empty()) {
                    out.push(Parsed::Event("system", format!("Model: {model}")));
                }
            }
            "tool_use" => out.push(Parsed::Event(
                "tool",
                describe_tool(
                    str_field(v, "name").unwrap_or("tool"),
                    v.get("input").unwrap_or(&Value::Null),
                ),
            )),
            "tool_result" => {
                let raw = str_field(v, "output").unwrap_or("");
                // Terminal results are JSON like {"output": "...", "exit_code": 0}.
                let (text, failed) = match serde_json::from_str::<Value>(raw) {
                    Ok(inner) if inner.get("output").is_some() => {
                        let code = inner.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
                        let text = content_text(&inner["output"]);
                        if code == 0 {
                            (text, false)
                        } else {
                            (format!("exit {code}\n{text}"), true)
                        }
                    }
                    _ => (raw.to_string(), false),
                };
                let output = clip(&text, TOOL_OUTPUT_CHARS);
                let error = failed || v.get("is_error").and_then(Value::as_bool) == Some(true);
                out.push(Parsed::Event(
                    if error { "tool_error" } else { "tool_result" },
                    output,
                ));
            }
            "result" => {
                if let Some(session) = str_field(v, "session_id").filter(|s| !s.is_empty()) {
                    out.push(Parsed::Session(session.into()));
                }
                if let Some(error) = str_field(v, "error") {
                    out.push(Parsed::Event("error", error.into()));
                }
                out.push(Parsed::Result {
                    text: str_field(v, "text")
                        .filter(|t| !t.is_empty())
                        .map(String::from),
                    is_error: v.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0,
                    cost_usd: None,
                    tokens: v.pointer("/tokens/total").and_then(Value::as_i64),
                });
            }
            _ => {}
        }
        out
    }
}

fn claude(v: &Value) -> Vec<Parsed> {
    let mut out = Vec::new();
    match (str_field(v, "type"), str_field(v, "subtype")) {
        (Some("system"), Some("init")) => {
            if let Some(session) = str_field(v, "session_id") {
                out.push(Parsed::Session(session.into()));
            }
            if let Some(model) = str_field(v, "model") {
                out.push(Parsed::Event("system", format!("Model: {model}")));
            }
        }
        (Some("assistant"), _) => {
            for part in v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match str_field(part, "type") {
                    Some("text") => {
                        let text = str_field(part, "text").unwrap_or("").trim();
                        if !text.is_empty() {
                            out.push(Parsed::Event("text", text.into()));
                        }
                    }
                    Some("tool_use") => out.push(Parsed::Event(
                        "tool",
                        describe_tool(
                            str_field(part, "name").unwrap_or("tool"),
                            part.get("input").unwrap_or(&Value::Null),
                        ),
                    )),
                    _ => {}
                }
            }
        }
        (Some("user"), _) => {
            for part in v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|p| str_field(p, "type") == Some("tool_result"))
            {
                let error = part.get("is_error").and_then(Value::as_bool) == Some(true);
                out.push(Parsed::Event(
                    if error { "tool_error" } else { "tool_result" },
                    clip(
                        &content_text(part.get("content").unwrap_or(&Value::Null)),
                        TOOL_OUTPUT_CHARS,
                    ),
                ));
            }
        }
        (Some("rate_limit_event"), _) => {
            let status = v
                .pointer("/rate_limit_info/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "allowed" {
                out.push(Parsed::Event(
                    "system",
                    format!("Rate limit: {}", clip(&v.to_string(), 500)),
                ));
            }
        }
        (Some("result"), subtype) => {
            if let Some(session) = str_field(v, "session_id") {
                out.push(Parsed::Session(session.into()));
            }
            let is_error = v.get("is_error").and_then(Value::as_bool) == Some(true);
            if is_error {
                out.push(Parsed::Event(
                    "error",
                    str_field(v, "result")
                        .map(String::from)
                        .unwrap_or_else(|| format!("Claude ended with {}", subtype.unwrap_or("?"))),
                ));
            }
            let tokens = v.get("usage").map(|u| {
                ["input_tokens", "output_tokens"]
                    .iter()
                    .filter_map(|k| u.get(*k).and_then(Value::as_i64))
                    .sum()
            });
            out.push(Parsed::Result {
                text: str_field(v, "result").map(String::from),
                is_error,
                cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                tokens,
            });
        }
        _ => {}
    }
    out
}

fn codex(v: &Value) -> Vec<Parsed> {
    let item = v.get("item").unwrap_or(&Value::Null);
    let item_type = str_field(item, "type").unwrap_or("");
    match (str_field(v, "type").unwrap_or(""), item_type) {
        ("thread.started", _) => str_field(v, "thread_id")
            .map(|id| vec![Parsed::Session(id.into())])
            .unwrap_or_default(),
        ("item.started", "command_execution") => vec![Parsed::Event(
            "tool",
            format!(
                "Shell: {}",
                clip(str_field(item, "command").unwrap_or(""), 500)
            ),
        )],
        ("item.completed", "agent_message") => {
            vec![Parsed::Event(
                "text",
                str_field(item, "text").unwrap_or("").trim().into(),
            )]
        }
        ("item.completed", "command_execution") => {
            let code = item.get("exit_code").and_then(Value::as_i64);
            let output = clip(
                str_field(item, "aggregated_output").unwrap_or(""),
                TOOL_OUTPUT_CHARS,
            );
            let kind = if code.unwrap_or(0) == 0 {
                "tool_result"
            } else {
                "tool_error"
            };
            vec![Parsed::Event(
                kind,
                match code {
                    Some(code) if code != 0 => format!("exit {code}\n{output}"),
                    _ => output,
                },
            )]
        }
        ("item.completed", "file_change") => {
            let files: Vec<String> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|c| {
                    format!(
                        "{} {}",
                        str_field(c, "kind").unwrap_or("edit"),
                        str_field(c, "path").unwrap_or("?")
                    )
                })
                .collect();
            vec![Parsed::Event("tool", format!("Edit: {}", files.join(", ")))]
        }
        ("item.completed", "mcp_tool_call" | "web_search") => vec![Parsed::Event(
            "tool",
            describe_tool(
                str_field(item, "tool")
                    .or(str_field(item, "query"))
                    .unwrap_or(item_type),
                item.get("arguments").unwrap_or(&Value::Null),
            ),
        )],
        ("item.completed", "todo_list") => {
            let items: Vec<String> = item
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| {
                    let done = t.get("completed").and_then(Value::as_bool) == Some(true);
                    format!(
                        "{} {}",
                        if done { "[x]" } else { "[ ]" },
                        str_field(t, "text").unwrap_or("")
                    )
                })
                .collect();
            vec![Parsed::Event("plan", items.join("\n"))]
        }
        ("item.completed", "error") | ("error", _) => vec![Parsed::Event(
            "error",
            str_field(item, "message")
                .or(str_field(v, "message"))
                .unwrap_or("error")
                .into(),
        )],
        ("turn.failed", _) => vec![
            Parsed::Event(
                "error",
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("turn failed")
                    .into(),
            ),
            Parsed::Result {
                text: None,
                is_error: true,
                cost_usd: None,
                tokens: None,
            },
        ],
        ("turn.completed", _) => {
            let tokens = v.get("usage").map(|u| {
                ["input_tokens", "output_tokens"]
                    .iter()
                    .filter_map(|k| u.get(*k).and_then(Value::as_i64))
                    .sum()
            });
            vec![Parsed::Result {
                text: None,
                is_error: false,
                cost_usd: None,
                tokens,
            }]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn input<'a>(file: &'a Path, workdir: &'a Path, resume: Option<&'a str>) -> TurnInput<'a> {
        TurnInput {
            text: "fix it",
            text_file: file,
            workdir,
            resume,
            model: Some("opus"),
            effort: Some("high"),
            feedback_id: 7,
            app: "demo",
        }
    }

    #[test]
    fn builds_agent_command_lines() {
        let (file, dir) = (PathBuf::from("/tmp/p.md"), PathBuf::from("/w"));
        let agent = AgentConfig::default();
        let claude = invocation("claude", &agent, &input(&file, &dir, Some("s1")));
        assert!(claude.live);
        assert!(claude.argv.windows(2).any(|w| w == ["--effort", "high"]));
        assert!(claude.argv.windows(2).any(|w| w == ["--resume", "s1"]));

        let codex = invocation("codex", &agent, &input(&file, &dir, Some("t1")));
        assert_eq!(&codex.argv[..3], ["codex", "exec", "resume"]);
        assert!(
            codex
                .argv
                .contains(&"model_reasoning_effort=\"high\"".into())
        );
        assert_eq!(codex.argv.last().unwrap(), "-");

        let hermes = invocation("hermes", &agent, &input(&file, &dir, None));
        assert!(hermes.argv.windows(2).any(|w| w == ["--reasoning", "high"]));
        assert!(!hermes.argv.contains(&"--resume".into()));

        let custom = AgentConfig {
            command: Some(vec!["run".into(), "{model}".into(), "{prompt_file}".into()]),
            ..Default::default()
        };
        let custom = invocation(&custom.kind("x"), &custom, &input(&file, &dir, None));
        assert_eq!(custom.argv, ["run", "opus", "/tmp/p.md"]);
    }

    #[test]
    fn parses_claude_stream() {
        let mut p = Parser::new("claude");
        assert_eq!(
            p.line(
                r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-opus"}"#
            ),
            vec![
                Parsed::Session("abc".into()),
                Parsed::Event("system", "Model: claude-opus".into())
            ]
        );
        assert_eq!(
            p.line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#),
            vec![
                Parsed::Event("text", "Looking".into()),
                Parsed::Event("tool", "Bash: ls".into())
            ]
        );
        assert_eq!(
            p.line(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"a\nb","is_error":false}]}}"#),
            vec![Parsed::Event("tool_result", "a\nb".into())]
        );
        let result = p.line(r#"{"type":"result","subtype":"success","is_error":false,"result":"Done","total_cost_usd":0.5,"session_id":"abc","usage":{"input_tokens":10,"output_tokens":5}}"#);
        assert_eq!(
            result.last().unwrap(),
            &Parsed::Result {
                text: Some("Done".into()),
                is_error: false,
                cost_usd: Some(0.5),
                tokens: Some(15)
            }
        );
    }

    #[test]
    fn parses_codex_stream() {
        let mut p = Parser::new("codex");
        assert_eq!(
            p.line(r#"{"type":"thread.started","thread_id":"t1"}"#),
            vec![Parsed::Session("t1".into())]
        );
        assert_eq!(
            p.line(r#"{"type":"item.completed","item":{"type":"command_execution","command":"x","aggregated_output":"boom","exit_code":2}}"#),
            vec![Parsed::Event("tool_error", "exit 2\nboom".into())]
        );
        assert!(matches!(
            p.line(r#"{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":4}}"#)[0],
            Parsed::Result {
                tokens: Some(7),
                ..
            }
        ));
    }

    #[test]
    fn joins_hermes_text_deltas() {
        let mut p = Parser::new("hermes");
        assert!(p.line(r#"{"type":"text","text":"Hel"}"#).is_empty());
        assert!(p.line(r#"{"type":"text","text":"lo"}"#).is_empty());
        assert_eq!(
            p.line(r#"{"type":"tool_use","name":"terminal","input":{"command":"ls"}}"#),
            vec![
                Parsed::Event("text", "Hello".into()),
                Parsed::Event("tool", "terminal: ls".into())
            ]
        );
        assert_eq!(
            p.line(r#"{"type":"tool_result","name":"terminal","output":"{\"output\": \"boom\", \"exit_code\": 1}"}"#),
            vec![Parsed::Event("tool_error", "exit 1\nboom".into())]
        );
        p.line(r#"{"type":"text","text":"bye"}"#);
        assert_eq!(p.finish(), vec![Parsed::Event("text", "bye".into())]);
    }
}
