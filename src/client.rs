//! Blocking HTTP client and the human/agent-facing CLI commands.

use crate::{
    apps::AppInfo,
    config::{ClientConfig, expand_home, home},
    db::{Event, Feedback, Screenshot},
    runs::{Command, Run, RunEvent, RunUpdate, WorkerStatus},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fmt::Write as _, path::PathBuf, time::Duration};

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

#[derive(Debug, Deserialize)]
pub struct Detail {
    #[serde(flatten)]
    pub feedback: Feedback,
    pub screenshots: Vec<Screenshot>,
    pub events: Vec<Event>,
    #[serde(default)]
    pub runs: Vec<Run>,
}

#[derive(Debug, Deserialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: Run,
    pub events: Vec<RunEvent>,
}

fn check(mut res: ureq::http::Response<ureq::Body>) -> Result<ureq::http::Response<ureq::Body>> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body = res.body_mut().read_to_string().unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"].as_str().map(String::from))
        .unwrap_or(body);
    bail!("feedback-hub returned {status}: {message}")
}

impl Client {
    pub fn new(config: &ClientConfig) -> Result<Self> {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Ok(Self {
            base: config.url(),
            token: config.token()?,
            agent,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let res = self
            .agent
            .get(self.url(path))
            .header("Authorization", self.auth())
            .call()
            .with_context(|| format!("GET {path}"))?;
        Ok(check(res)?.body_mut().read_json()?)
    }

    fn send(&self, method: &str, path: &str, body: Value) -> Result<Option<Value>> {
        let req = match method {
            "PATCH" => self.agent.patch(self.url(path)),
            _ => self.agent.post(self.url(path)),
        };
        let res = req
            .header("Authorization", self.auth())
            .send_json(&body)
            .with_context(|| format!("{method} {path}"))?;
        let mut res = check(res)?;
        if res.status() == 204 {
            return Ok(None);
        }
        let text = res.body_mut().read_to_string()?;
        Ok(serde_json::from_str(&text).ok())
    }

    pub fn detail(&self, id: i64) -> Result<Detail> {
        self.get(&format!("/api/feedback/{id}"))
    }

    pub fn list(&self, status: &str, app: Option<&str>, limit: i64) -> Result<Vec<Feedback>> {
        let mut path = format!("/api/feedback?status={status}&limit={limit}");
        if let Some(app) = app {
            path.push_str(&format!("&app={app}"));
        }
        self.get(&path)
    }

    pub fn apps(&self) -> Result<Vec<AppInfo>> {
        self.get("/api/apps")
    }

    pub fn screenshot(&self, id: i64, idx: i64) -> Result<Vec<u8>> {
        let res = self
            .agent
            .get(self.url(&format!("/api/feedback/{id}/screenshots/{idx}")))
            .header("Authorization", self.auth())
            .call()?;
        Ok(check(res)?
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()?)
    }

    pub fn comment(&self, id: i64, author: &str, kind: &str, body: &str) -> Result<()> {
        self.send(
            "POST",
            &format!("/api/feedback/{id}/events"),
            json!({ "author": author, "kind": kind, "body": body }),
        )?;
        Ok(())
    }

    pub fn set_status(
        &self,
        id: i64,
        status: &str,
        author: &str,
        note: Option<&str>,
    ) -> Result<()> {
        self.send(
            "PATCH",
            &format!("/api/feedback/{id}"),
            json!({ "status": status, "author": author, "note": note }),
        )?;
        Ok(())
    }

    pub fn queue(
        &self,
        id: i64,
        run: &QueueOptions,
        author: &str,
        note: Option<&str>,
    ) -> Result<()> {
        self.send(
            "POST",
            &format!("/api/feedback/{id}/queue"),
            json!({ "agent": run.agent, "model": run.model, "effort": run.effort, "author": author, "note": note }),
        )?;
        Ok(())
    }

    pub fn claim_run(
        &self,
        worker: &str,
        agents: &[String],
        default_agent: &str,
        include_open: bool,
    ) -> Result<Option<Run>> {
        let value = self.send(
            "POST",
            "/api/worker/claim-run",
            json!({
                "worker": worker,
                "agents": agents,
                "defaultAgent": default_agent,
                "includeOpen": include_open,
            }),
        )?;
        Ok(value.map(serde_json::from_value).transpose()?)
    }

    pub fn heartbeat(&self, status: &WorkerStatus) -> Result<()> {
        self.send(
            "POST",
            "/api/worker/heartbeat",
            serde_json::to_value(status)?,
        )?;
        Ok(())
    }

    pub fn runs(&self, active_only: bool, feedback: Option<i64>, limit: i64) -> Result<Vec<Run>> {
        let mut path = format!("/api/runs?active={active_only}&limit={limit}");
        if let Some(id) = feedback {
            path.push_str(&format!("&feedback={id}"));
        }
        self.get(&path)
    }

    pub fn run(&self, id: i64, after: i64) -> Result<RunDetail> {
        self.get(&format!("/api/runs/{id}?after={after}"))
    }

    pub fn update_run(&self, id: i64, update: &RunUpdate) -> Result<()> {
        self.send(
            "PATCH",
            &format!("/api/runs/{id}"),
            serde_json::to_value(update)?,
        )?;
        Ok(())
    }

    pub fn add_run_events(&self, id: i64, events: &[(String, String)]) -> Result<()> {
        let events: Vec<Value> = events
            .iter()
            .map(|(kind, body)| json!({ "kind": kind, "body": body }))
            .collect();
        self.send(
            "POST",
            &format!("/api/runs/{id}/events"),
            Value::Array(events),
        )?;
        Ok(())
    }

    pub fn command(
        &self,
        id: i64,
        kind: &str,
        body: &str,
        interrupt: bool,
        author: &str,
    ) -> Result<Run> {
        let value = self.send(
            "POST",
            &format!("/api/runs/{id}/commands"),
            json!({ "kind": kind, "body": body, "interrupt": interrupt, "author": author }),
        )?;
        Ok(serde_json::from_value(value.context("empty response")?)?)
    }

    pub fn take_commands(&self, id: i64) -> Result<Vec<Command>> {
        Ok(self
            .send("POST", &format!("/api/runs/{id}/commands/take"), json!({}))?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default())
    }

    /// Downloads screenshots into `dir` and returns their paths.
    pub fn download_screenshots(
        &self,
        detail: &Detail,
        dir: &std::path::Path,
    ) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dir)?;
        detail
            .screenshots
            .iter()
            .map(|shot| {
                let path = dir.join(format!(
                    "screenshot-{}.{}",
                    shot.idx + 1,
                    crate::db::extension(&shot.mime)
                ));
                if !path.exists() {
                    std::fs::write(&path, self.screenshot(detail.feedback.id, shot.idx)?)?;
                }
                Ok(path)
            })
            .collect()
    }
}

/// Agent, model and reasoning effort for a run; `None` means the agent's default.
#[derive(Debug, Default, Clone)]
pub struct QueueOptions {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub fn cache_dir(id: i64) -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join(format!("feedback-hub/{id}"))
}

/// The author recorded on comments/status changes made from the CLI.
pub fn default_author() -> String {
    std::env::var("FEEDBACK_HUB_AUTHOR")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "cli".into())
}

fn context_lines(context: &Value, out: &mut String) {
    let Some(map) = context.as_object() else {
        return;
    };
    for (key, value) in map {
        if matches!(key.as_str(), "logs" | "network") {
            continue;
        }
        let rendered = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let _ = writeln!(out, "- {key}: {rendered}");
    }
}

fn log_section(context: &Value, key: &str, title: &str, out: &mut String) {
    let Some(entries) = context
        .get(key)
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    else {
        return;
    };
    let _ = writeln!(out, "\n### {title}\n\n```");
    for entry in entries {
        match entry {
            Value::String(s) => {
                let _ = writeln!(out, "{s}");
            }
            Value::Object(o) => {
                let line = ["at", "level", "method", "status", "url", "message"]
                    .iter()
                    .filter_map(|k| o.get(*k))
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = writeln!(out, "{line}");
            }
            other => {
                let _ = writeln!(out, "{other}");
            }
        }
    }
    let _ = writeln!(out, "```");
}

/// Renders an item as Markdown for humans and agents.
pub fn render_markdown(detail: &Detail, app: Option<&AppInfo>, screenshots: &[PathBuf]) -> String {
    let f = &detail.feedback;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "## Feedback #{} — {} ({}, {})",
        f.id, f.app, f.kind, f.status
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Reported: {} via {}{}",
        f.created_at,
        f.source,
        f.reporter
            .as_deref()
            .map(|r| format!(" by {r}"))
            .unwrap_or_default()
    );
    if let Some(url) = &f.page_url {
        let _ = writeln!(
            out,
            "- Page: {url}{}",
            f.page_title
                .as_deref()
                .map(|t| format!(" (\"{t}\")"))
                .unwrap_or_default()
        );
    }
    if let Some(ua) = &f.user_agent {
        let _ = writeln!(out, "- User agent: {ua}");
    }
    context_lines(&f.context, &mut out);
    if let Some(app) = app {
        if let Some(repo) = &app.repo {
            let _ = writeln!(out, "- Repository: {repo}");
        }
        if let Some(notes) = &app.notes {
            let _ = writeln!(out, "- App notes: {notes}");
        }
    }
    let _ = writeln!(out, "\n### Message\n");
    if f.message.is_empty() {
        let _ = writeln!(out, "_(no text; see screenshots)_");
    } else {
        for line in f.message.lines() {
            let _ = writeln!(out, "> {line}");
        }
    }
    if !screenshots.is_empty() {
        let _ = writeln!(out, "\n### Screenshots\n");
        let _ = writeln!(
            out,
            "Red/colored marks were drawn by the reporter to highlight the problem.\n"
        );
        let _ = writeln!(
            out,
            "The reporter may refer to them as #1, #2, … in the message.\n"
        );
        for (i, path) in screenshots.iter().enumerate() {
            let note = detail
                .screenshots
                .get(i)
                .and_then(|s| s.note.as_deref())
                .map(|n| format!(" — reporter's note: \"{n}\""))
                .unwrap_or_default();
            let _ = writeln!(out, "- #{} {}{note}", i + 1, path.display());
        }
    } else if f.screenshot_count > 0 {
        let _ = writeln!(
            out,
            "\n{} screenshot(s) attached (download with `feedback-hub show {}`).",
            f.screenshot_count, f.id
        );
    }
    log_section(&f.context, "logs", "Recent console messages", &mut out);
    log_section(&f.context, "network", "Recent failed requests", &mut out);
    if !detail.events.is_empty() {
        let _ = writeln!(out, "\n### Timeline\n");
        for event in &detail.events {
            let _ = writeln!(
                out,
                "- {} [{}] {}: {}",
                event.at,
                event.kind,
                event.author,
                event.body.replace('\n', "\n  ")
            );
        }
    }
    out
}

pub fn find_app<'a>(apps: &'a [AppInfo], id: &str) -> Option<&'a AppInfo> {
    apps.iter().find(|a| a.id == id)
}

pub fn print_list(items: &[Feedback]) {
    if items.is_empty() {
        println!("No feedback matches.");
        return;
    }
    for f in items {
        let first_line: String = f
            .message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(90)
            .collect();
        println!(
            "#{:<5} {:<12} {:<16} {:<6} {}{}  {}",
            f.id,
            f.status,
            f.app,
            f.kind,
            if first_line.is_empty() {
                "(screenshot only)".to_string()
            } else {
                first_line
            },
            if f.screenshot_count > 0 {
                format!("  [{} img]", f.screenshot_count)
            } else {
                String::new()
            },
            &f.created_at[..10.min(f.created_at.len())],
        );
    }
}

pub fn resolve_dir(dir: Option<String>, id: i64) -> PathBuf {
    dir.map(|d| expand_home(&d))
        .unwrap_or_else(|| cache_dir(id))
}

pub fn print_runs(runs: &[Run]) {
    if runs.is_empty() {
        println!("No runs.");
        return;
    }
    for r in runs {
        let what = [
            Some(r.agent.as_str()),
            r.model.as_deref(),
            r.effort.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("/");
        let title: String = r.title.chars().take(60).collect();
        println!(
            "run {:<5} {:<10} #{:<5} {:<14} {:<24} {:<10} {}",
            r.id,
            r.status,
            r.feedback_id,
            r.app,
            what,
            r.worker.as_deref().unwrap_or("-"),
            title
        );
    }
}

pub fn print_run_events(events: &[RunEvent]) {
    for e in events {
        let time = e.at.get(11..19).unwrap_or(&e.at);
        println!("{time} [{}] {}", e.kind, e.body.replace('\n', "\n    "));
    }
}
