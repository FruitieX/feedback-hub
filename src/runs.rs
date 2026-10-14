//! Agent runs: one coding-agent session working on a feedback item, with its
//! streamed activity log and the steering commands sent to it from the UI.
//!
//! A run is claimed by a worker, which streams events back and polls for
//! commands. Sending a message to a finished run queues it again on the same
//! worker so the agent's session can be resumed there.

use crate::db::{Database, now};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const RUN_STATUSES: &[&str] = &["queued", "running", "succeeded", "failed", "stopped"];
pub const ACTIVE: &[&str] = &["queued", "running"];

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feedback_id INTEGER NOT NULL REFERENCES feedback(id) ON DELETE CASCADE,
    agent TEXT NOT NULL,
    model TEXT,
    effort TEXT,
    note TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    worker TEXT,
    workdir TEXT,
    branch TEXT,
    session_id TEXT,
    log_path TEXT,
    summary TEXT,
    cost_usd REAL,
    tokens INTEGER,
    requested_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS runs_status ON runs(status, id);
CREATE INDEX IF NOT EXISTS runs_feedback ON runs(feedback_id, id);
CREATE TABLE IF NOT EXISTS run_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS run_events_run ON run_events(run_id, id);
CREATE TABLE IF NOT EXISTS run_commands (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    at TEXT NOT NULL,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    interrupt INTEGER NOT NULL DEFAULT 0,
    delivered_at TEXT
);
CREATE INDEX IF NOT EXISTS run_commands_run ON run_commands(run_id, id);
";

/// Items queued before runs existed get a run so the worker still picks them up.
pub fn migrate(conn: &Connection) -> Result<()> {
    let at = now();
    conn.execute(
        "INSERT INTO runs (feedback_id, agent, status, requested_by, created_at, updated_at)
         SELECT id, COALESCE(agent, 'claude'), 'queued', 'migration', ?1, ?1 FROM feedback f
         WHERE status = 'queued'
           AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.feedback_id = f.id AND r.status IN ('queued', 'running'))",
        [at],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub id: i64,
    pub feedback_id: i64,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Extra instructions given when the run was queued.
    pub note: Option<String>,
    pub status: String,
    pub worker: Option<String>,
    pub workdir: Option<String>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    pub log_path: Option<String>,
    pub summary: Option<String>,
    pub cost_usd: Option<f64>,
    pub tokens: Option<i64>,
    pub requested_by: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub updated_at: String,
    pub app: String,
    /// First line of the feedback message.
    pub title: String,
    pub feedback_status: String,
    /// Messages sent to the agent that it has not received yet.
    pub pending_messages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunEvent {
    pub id: i64,
    pub at: String,
    pub kind: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Command {
    pub id: i64,
    pub kind: String,
    pub body: String,
    pub interrupt: bool,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    /// What the CLI accepts, e.g. `opus` or `gpt-6-sol`.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Reasoning efforts this model supports (empty: no effort setting).
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

/// What a worker can run, reported with its heartbeat so the UI can offer it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    /// Selectable models as discovered from the CLI (empty: unknown, use the default).
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Efforts valid for the agent's default model.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// Label of the model used when none is chosen, e.g. "Opus 5.5".
    #[serde(default)]
    pub default_model: Option<String>,
    /// Why models could not be discovered.
    #[serde(default)]
    pub models_error: Option<String>,
    /// Messages reach the agent mid-turn instead of after the current turn.
    pub live_steering: bool,
}

impl AgentInfo {
    /// Checks a model/effort choice against what this agent reported.
    pub fn validate(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), String> {
        let efforts = match model {
            Some(model) if !self.models.is_empty() => {
                let Some(info) = self.models.iter().find(|m| m.id == model) else {
                    return Err(format!(
                        "{} does not offer model {model:?} (available: {})",
                        self.name,
                        self.models
                            .iter()
                            .map(|m| m.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                };
                if info.efforts.is_empty() && effort.is_some() {
                    return Err(format!(
                        "{} model {model:?} has no reasoning effort setting",
                        self.name
                    ));
                }
                &info.efforts
            }
            Some(_) => return Ok(()),
            None => &self.efforts,
        };
        match effort {
            Some(effort) if !efforts.is_empty() && !efforts.iter().any(|e| e == effort) => {
                Err(format!(
                    "effort {effort:?} is not available for {} {} (available: {})",
                    self.name,
                    model.unwrap_or("default model"),
                    efforts.join(", ")
                ))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerStatus {
    pub name: String,
    pub agents: Vec<AgentInfo>,
    pub running: Vec<i64>,
    pub max_parallel: usize,
    #[serde(default)]
    pub last_seen: String,
}

pub struct NewRun {
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub note: Option<String>,
}

/// Fields a worker reports while running; `None` leaves a field unchanged.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RunUpdate {
    pub status: Option<String>,
    pub workdir: Option<String>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    pub log_path: Option<String>,
    pub summary: Option<String>,
    pub cost_usd: Option<f64>,
    pub tokens: Option<i64>,
}

pub struct RunFilter {
    pub active_only: bool,
    pub feedback_id: Option<i64>,
    pub limit: i64,
}

const RUN_COLUMNS: &str = "r.id, r.feedback_id, r.agent, r.model, r.effort, r.note, r.status, r.worker, r.workdir, r.branch, r.session_id, r.log_path, r.summary, r.cost_usd, r.tokens, r.requested_by, r.created_at, r.started_at, r.finished_at, r.updated_at, f.app, f.message, f.status,
    (SELECT COUNT(*) FROM run_commands c WHERE c.run_id = r.id AND c.delivered_at IS NULL AND c.kind = 'message')
    FROM runs r JOIN feedback f ON f.id = r.feedback_id";

fn row_to_run(row: &rusqlite::Row) -> rusqlite::Result<Run> {
    let message: String = row.get(21)?;
    Ok(Run {
        id: row.get(0)?,
        feedback_id: row.get(1)?,
        agent: row.get(2)?,
        model: row.get(3)?,
        effort: row.get(4)?,
        note: row.get(5)?,
        status: row.get(6)?,
        worker: row.get(7)?,
        workdir: row.get(8)?,
        branch: row.get(9)?,
        session_id: row.get(10)?,
        log_path: row.get(11)?,
        summary: row.get(12)?,
        cost_usd: row.get(13)?,
        tokens: row.get(14)?,
        requested_by: row.get(15)?,
        created_at: row.get(16)?,
        started_at: row.get(17)?,
        finished_at: row.get(18)?,
        updated_at: row.get(19)?,
        app: row.get(20)?,
        title: message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(140)
            .collect(),
        feedback_status: row.get(22)?,
        pending_messages: row.get(23)?,
    })
}

fn get_run_tx(conn: &Connection, id: i64) -> Result<Option<Run>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} WHERE r.id = ?1"),
            [id],
            row_to_run,
        )
        .optional()?)
}

fn add_run_event_tx(conn: &Connection, run_id: i64, kind: &str, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO run_events (run_id, at, kind, body) VALUES (?1, ?2, ?3, ?4)",
        params![run_id, now(), kind, body],
    )?;
    Ok(())
}

fn describe(agent: &str, model: Option<&str>, effort: Option<&str>) -> String {
    let mut text = agent.to_string();
    let extras: Vec<&str> = [model, effort].into_iter().flatten().collect();
    if !extras.is_empty() {
        text.push_str(&format!(" ({})", extras.join(", ")));
    }
    text
}

impl Database {
    /// Queues a new run. Returns `Err(run id)` when the item already has an active run.
    pub fn create_run(
        &self,
        feedback_id: i64,
        new: &NewRun,
        author: &str,
    ) -> Result<std::result::Result<Run, i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT id FROM runs WHERE feedback_id = ?1 AND status IN ('queued', 'running') ORDER BY id DESC LIMIT 1",
                [feedback_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(active) = active {
            return Ok(Err(active));
        }
        let at = now();
        tx.execute(
            "INSERT INTO runs (feedback_id, agent, model, effort, note, requested_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![feedback_id, new.agent, new.model, new.effort, new.note, author, at],
        )?;
        let run_id = tx.last_insert_rowid();
        let what = describe(&new.agent, new.model.as_deref(), new.effort.as_deref());
        let mut note = format!("Run #{run_id}: {what}");
        if let Some(extra) = new.note.as_deref().filter(|n| !n.trim().is_empty()) {
            note.push_str(&format!("\n\n{}", extra.trim()));
        }
        Self::set_status_tx(
            &tx,
            feedback_id,
            "queued",
            author,
            Some(&note),
            Some(&new.agent),
        )?;
        add_run_event_tx(&tx, run_id, "status", &format!("Queued {what} by {author}"))?;
        let run = get_run_tx(&tx, run_id)?.expect("just inserted");
        tx.commit()?;
        Ok(Ok(run))
    }

    pub fn get_run(&self, id: i64) -> Result<Option<Run>> {
        get_run_tx(&self.conn(), id)
    }

    pub fn list_runs(&self, filter: &RunFilter) -> Result<Vec<Run>> {
        let mut sql = format!("SELECT {RUN_COLUMNS} WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if filter.active_only {
            sql.push_str(" AND r.status IN ('queued', 'running')");
        }
        if let Some(id) = filter.feedback_id {
            sql.push_str(" AND r.feedback_id = ?");
            args.push(id.into());
        }
        sql.push_str(" ORDER BY r.id DESC LIMIT ?");
        args.push(filter.limit.clamp(1, 500).into());
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), row_to_run)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn run_events(&self, run_id: i64, after: i64, limit: i64) -> Result<Vec<RunEvent>> {
        let conn = self.conn();
        // The newest `limit` events after `after`, oldest first.
        let mut stmt = conn.prepare(
            "SELECT id, at, kind, body FROM (
                SELECT id, at, kind, body FROM run_events WHERE run_id = ?1 AND id > ?2 ORDER BY id DESC LIMIT ?3
             ) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![run_id, after, limit.clamp(1, 5000)], |row| {
            Ok(RunEvent {
                id: row.get(0)?,
                at: row.get(1)?,
                kind: row.get(2)?,
                body: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_run_events(&self, run_id: i64, events: &[(String, String)]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (kind, body) in events {
            add_run_event_tx(&tx, run_id, kind, body)?;
        }
        tx.execute(
            "UPDATE runs SET updated_at = ?2 WHERE id = ?1",
            params![run_id, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_run(&self, id: i64, update: &RunUpdate) -> Result<Option<Run>> {
        if let Some(status) = &update.status
            && !RUN_STATUSES.contains(&status.as_str())
        {
            bail!("unknown run status {status:?}");
        }
        let finished = update
            .status
            .as_deref()
            .is_some_and(|s| !ACTIVE.contains(&s));
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let at = now();
        tx.execute(
            "UPDATE runs SET
                status = COALESCE(?2, status),
                workdir = COALESCE(?3, workdir),
                branch = COALESCE(?4, branch),
                session_id = COALESCE(?5, session_id),
                log_path = COALESCE(?6, log_path),
                summary = COALESCE(?7, summary),
                cost_usd = COALESCE(?8, cost_usd),
                tokens = COALESCE(?9, tokens),
                finished_at = CASE WHEN ?10 THEN ?11 ELSE finished_at END,
                updated_at = ?11
             WHERE id = ?1",
            params![
                id,
                update.status,
                update.workdir,
                update.branch,
                update.session_id,
                update.log_path,
                update.summary,
                update.cost_usd,
                update.tokens,
                finished,
                at
            ],
        )?;
        if let Some(status) = &update.status {
            add_run_event_tx(&tx, id, "status", &format!("Run {status}"))?;
        }
        let run = get_run_tx(&tx, id)?;
        tx.commit()?;
        Ok(run)
    }

    /// Records a steering command. A message to a finished run queues it again
    /// on its worker so the session can be resumed; stopping a queued run cancels it.
    pub fn add_command(
        &self,
        run_id: i64,
        kind: &str,
        body: &str,
        interrupt: bool,
        author: &str,
    ) -> Result<Run> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some(run) = get_run_tx(&tx, run_id)? else {
            bail!("run #{run_id} not found");
        };
        let at = now();
        let insert = |tx: &Connection| -> Result<()> {
            tx.execute(
                "INSERT INTO run_commands (run_id, at, author, kind, body, interrupt) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![run_id, at, author, kind, body, interrupt],
            )?;
            Ok(())
        };
        match (kind, run.status.as_str()) {
            ("stop", "queued") => {
                tx.execute(
                    "UPDATE runs SET status = 'stopped', finished_at = ?2, updated_at = ?2 WHERE id = ?1",
                    params![run_id, at],
                )?;
                add_run_event_tx(&tx, run_id, "status", &format!("Cancelled by {author}"))?;
                if run.feedback_status == "queued" {
                    Self::set_status_tx(
                        &tx,
                        run.feedback_id,
                        "open",
                        author,
                        Some(&format!("Run #{run_id} cancelled before it started.")),
                        None,
                    )?;
                }
            }
            ("stop", "running") => {
                insert(&tx)?;
                add_run_event_tx(
                    &tx,
                    run_id,
                    "status",
                    &format!("Stop requested by {author}"),
                )?;
            }
            ("stop", _) => bail!("run #{run_id} is not active"),
            ("message", status) => {
                if !ACTIVE.contains(&status) {
                    if run.session_id.is_none() {
                        bail!("run #{run_id} has no agent session to continue");
                    }
                    tx.execute(
                        "UPDATE runs SET status = 'queued', finished_at = NULL, updated_at = ?2 WHERE id = ?1",
                        params![run_id, at],
                    )?;
                    Self::set_status_tx(
                        &tx,
                        run.feedback_id,
                        "queued",
                        author,
                        Some(&format!("Follow-up for run #{run_id}:\n\n{body}")),
                        None,
                    )?;
                }
                insert(&tx)?;
                let label = if interrupt {
                    "user (interrupt)"
                } else {
                    "user"
                };
                add_run_event_tx(&tx, run_id, label, body)?;
            }
            _ => bail!("unknown command {kind:?}"),
        }
        let run = get_run_tx(&tx, run_id)?.expect("exists");
        tx.commit()?;
        Ok(run)
    }

    /// Hands undelivered commands to the worker and marks them delivered.
    pub fn take_commands(&self, run_id: i64) -> Result<Vec<Command>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let commands: Vec<Command> = {
            let mut stmt = tx.prepare(
                "SELECT id, kind, body, interrupt, author FROM run_commands WHERE run_id = ?1 AND delivered_at IS NULL ORDER BY id",
            )?;
            let rows = stmt.query_map([run_id], |row| {
                Ok(Command {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    body: row.get(2)?,
                    interrupt: row.get(3)?,
                    author: row.get(4)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        tx.execute(
            "UPDATE run_commands SET delivered_at = ?2 WHERE run_id = ?1 AND delivered_at IS NULL",
            params![run_id, now()],
        )?;
        tx.commit()?;
        Ok(commands)
    }

    /// Atomically hands the oldest queued run this worker can execute to it.
    /// Continued runs only go back to the worker that holds their session.
    pub fn claim_run(
        &self,
        worker: &str,
        agents: &[String],
        default_agent: &str,
        include_open: bool,
    ) -> Result<Option<Run>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let candidates: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, agent FROM runs WHERE status = 'queued' AND (worker IS NULL OR worker = ?1) ORDER BY id",
            )?;
            let rows = stmt.query_map([worker], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut pick = candidates
            .into_iter()
            .find(|(_, agent)| agents.contains(agent))
            .map(|(id, _)| id);
        if pick.is_none() && include_open && agents.iter().any(|a| a == default_agent) {
            let fresh: Option<i64> = tx
                .query_row(
                    "SELECT id FROM feedback f WHERE status = 'open' AND attempts = 0
                     AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.feedback_id = f.id) ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(feedback_id) = fresh {
                let at = now();
                tx.execute(
                    "INSERT INTO runs (feedback_id, agent, requested_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![feedback_id, default_agent, worker, at],
                )?;
                pick = Some(tx.last_insert_rowid());
            }
        }
        let Some(run_id) = pick else {
            return Ok(None);
        };
        let run = get_run_tx(&tx, run_id)?.expect("exists");
        let at = now();
        tx.execute(
            "UPDATE runs SET status = 'running', worker = ?2, started_at = COALESCE(started_at, ?3), updated_at = ?3 WHERE id = ?1",
            params![run_id, worker, at],
        )?;
        let resumed = run.session_id.is_some();
        tx.execute(
            "UPDATE feedback SET status = 'in_progress', agent = ?2, attempts = attempts + ?3 WHERE id = ?1",
            params![run.feedback_id, run.agent, if resumed { 0 } else { 1 }],
        )?;
        let what = describe(&run.agent, run.model.as_deref(), run.effort.as_deref());
        let body = if resumed {
            format!("Worker {worker} resumed run #{run_id} ({what})")
        } else {
            format!("Worker {worker} started run #{run_id} ({what})")
        };
        Self::add_event_tx(&tx, run.feedback_id, worker, "claimed", &body)?;
        add_run_event_tx(&tx, run_id, "status", &format!("Started on {worker}"))?;
        let run = get_run_tx(&tx, run_id)?;
        tx.commit()?;
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::NewFeedback;

    fn db_with_item() -> (Database, i64, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), "DELETE").unwrap();
        let id = db
            .create(NewFeedback {
                app: "demo".into(),
                kind: "bug".into(),
                message: "Broken\nmore".into(),
                reporter: None,
                page_url: None,
                page_title: None,
                user_agent: None,
                source: "web".into(),
                context: serde_json::json!({}),
                screenshots: vec![],
            })
            .unwrap();
        (db, id, dir)
    }

    fn new_run(agent: &str) -> NewRun {
        NewRun {
            agent: agent.into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            note: None,
        }
    }

    #[test]
    fn run_lifecycle_with_follow_up() {
        let (db, id, _dir) = db_with_item();
        let run = db
            .create_run(id, &new_run("claude"), "me")
            .unwrap()
            .unwrap();
        assert_eq!(run.title, "Broken");
        assert_eq!(db.get(id).unwrap().unwrap().status, "queued");
        assert_eq!(
            db.create_run(id, &new_run("codex"), "me").unwrap(),
            Err(run.id)
        );

        let claimed = |worker: &str| {
            db.claim_run(worker, &["claude".into()], "claude", false)
                .unwrap()
        };
        let started = claimed("w1").unwrap();
        assert_eq!(started.status, "running");
        assert_eq!(db.get(id).unwrap().unwrap().attempts, 1);
        assert!(claimed("w1").is_none());

        db.add_command(run.id, "message", "Also check dark mode", false, "me")
            .unwrap();
        let commands = db.take_commands(run.id).unwrap();
        assert_eq!(commands.len(), 1);
        assert!(db.take_commands(run.id).unwrap().is_empty());

        db.update_run(
            run.id,
            &RunUpdate {
                status: Some("succeeded".into()),
                session_id: Some("abc".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let continued = db
            .add_command(run.id, "message", "Please also add a test", false, "me")
            .unwrap();
        assert_eq!(continued.status, "queued");
        assert_eq!(continued.pending_messages, 1);
        assert!(claimed("w2").is_none(), "session lives on w1");
        let resumed = claimed("w1").unwrap();
        assert_eq!(resumed.session_id.as_deref(), Some("abc"));
        assert_eq!(
            db.get(id).unwrap().unwrap().attempts,
            1,
            "resuming is not a new attempt"
        );
        let kinds: Vec<String> = db
            .run_events(run.id, 0, 100)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds.contains(&"user".to_string()));
    }

    #[test]
    fn stopping_a_queued_run_cancels_it() {
        let (db, id, _dir) = db_with_item();
        let run = db.create_run(id, &new_run("codex"), "me").unwrap().unwrap();
        let stopped = db.add_command(run.id, "stop", "", false, "me").unwrap();
        assert_eq!(stopped.status, "stopped");
        assert_eq!(db.get(id).unwrap().unwrap().status, "open");
        assert!(db.add_command(run.id, "stop", "", false, "me").is_err());
        assert!(
            db.add_command(run.id, "message", "hi", false, "me")
                .is_err(),
            "no session to continue"
        );
    }

    #[test]
    fn claims_only_supported_agents_and_open_items_when_asked() {
        let (db, id, _dir) = db_with_item();
        db.create_run(id, &new_run("hermes"), "me")
            .unwrap()
            .unwrap();
        assert!(
            db.claim_run("w", &["claude".into()], "claude", true)
                .unwrap()
                .is_none()
        );
        let (db, _id, _dir) = db_with_item();
        let run = db
            .claim_run("w", &["claude".into()], "claude", true)
            .unwrap()
            .unwrap();
        assert_eq!(run.agent, "claude");
        assert_eq!(run.model, None);
    }
}
