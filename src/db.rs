//! SQLite storage for feedback items, their screenshots and timeline events.
//!
//! Screenshot bytes live on disk next to the database so the SQLite file stays
//! small enough to back up and inspect casually.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

pub const STATUSES: &[&str] = &["open", "queued", "in_progress", "resolved", "dismissed"];

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    screenshot_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Feedback {
    pub id: i64,
    pub app: String,
    pub kind: String,
    pub message: String,
    pub reporter: Option<String>,
    pub page_url: Option<String>,
    pub page_title: Option<String>,
    pub user_agent: Option<String>,
    pub source: String,
    pub context: serde_json::Value,
    pub status: String,
    pub agent: Option<String>,
    pub attempts: i64,
    pub screenshot_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Screenshot {
    pub idx: i64,
    pub mime: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: i64,
    pub at: String,
    pub author: String,
    pub kind: String,
    pub body: String,
}

pub struct NewFeedback {
    pub app: String,
    pub kind: String,
    pub message: String,
    pub reporter: Option<String>,
    pub page_url: Option<String>,
    pub page_title: Option<String>,
    pub user_agent: Option<String>,
    pub source: String,
    pub context: serde_json::Value,
    pub screenshots: Vec<(String, Vec<u8>)>,
}

#[derive(Default)]
pub struct ListFilter {
    pub statuses: Vec<String>,
    pub app: Option<String>,
    pub limit: i64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    app TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    reporter TEXT,
    page_url TEXT,
    page_title TEXT,
    user_agent TEXT,
    source TEXT NOT NULL,
    context TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'open',
    agent TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS feedback_status ON feedback(status, id);
CREATE TABLE IF NOT EXISTS screenshots (
    feedback_id INTEGER NOT NULL REFERENCES feedback(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    mime TEXT NOT NULL,
    size INTEGER NOT NULL,
    PRIMARY KEY (feedback_id, idx)
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feedback_id INTEGER NOT NULL REFERENCES feedback(id) ON DELETE CASCADE,
    at TEXT NOT NULL,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_feedback ON events(feedback_id, id);
";

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

const FEEDBACK_COLUMNS: &str = "id, app, kind, message, reporter, page_url, page_title, user_agent, source, context, status, agent, attempts, created_at, updated_at, (SELECT COUNT(*) FROM screenshots s WHERE s.feedback_id = feedback.id)";

fn row_to_feedback(row: &rusqlite::Row) -> rusqlite::Result<Feedback> {
    let context: String = row.get(9)?;
    Ok(Feedback {
        id: row.get(0)?,
        app: row.get(1)?,
        kind: row.get(2)?,
        message: row.get(3)?,
        reporter: row.get(4)?,
        page_url: row.get(5)?,
        page_title: row.get(6)?,
        user_agent: row.get(7)?,
        source: row.get(8)?,
        context: serde_json::from_str(&context).unwrap_or(serde_json::Value::Null),
        status: row.get(10)?,
        agent: row.get(11)?,
        attempts: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        screenshot_count: row.get(15)?,
    })
}

impl Database {
    pub fn open(data_dir: &Path, journal_mode: &str) -> Result<Self> {
        let screenshot_dir = data_dir.join("screenshots");
        std::fs::create_dir_all(&screenshot_dir)
            .with_context(|| format!("creating {}", screenshot_dir.display()))?;
        let conn = Connection::open(data_dir.join("feedback.db"))?;
        // NFS-backed volumes cannot use WAL, so the journal mode is configurable.
        conn.pragma_update(None, "journal_mode", journal_mode)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            screenshot_dir,
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn screenshot_path(&self, id: i64, idx: i64, mime: &str) -> PathBuf {
        self.screenshot_dir
            .join(format!("{id}-{idx}.{}", extension(mime)))
    }

    pub fn create(&self, new: NewFeedback) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let at = now();
        tx.execute(
            "INSERT INTO feedback (app, kind, message, reporter, page_url, page_title, user_agent, source, context, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                new.app,
                new.kind,
                new.message,
                new.reporter,
                new.page_url,
                new.page_title,
                new.user_agent,
                new.source,
                new.context.to_string(),
                at
            ],
        )?;
        let id = tx.last_insert_rowid();
        for (idx, (mime, bytes)) in new.screenshots.iter().enumerate() {
            let idx = idx as i64;
            std::fs::write(self.screenshot_path(id, idx, mime), bytes)?;
            tx.execute(
                "INSERT INTO screenshots (feedback_id, idx, mime, size) VALUES (?1, ?2, ?3, ?4)",
                params![id, idx, mime, bytes.len() as i64],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn get(&self, id: i64) -> Result<Option<Feedback>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {FEEDBACK_COLUMNS} FROM feedback WHERE id = ?1"),
                [id],
                row_to_feedback,
            )
            .optional()?)
    }

    pub fn list(&self, filter: &ListFilter) -> Result<Vec<Feedback>> {
        let mut sql = format!("SELECT {FEEDBACK_COLUMNS} FROM feedback WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if !filter.statuses.is_empty() {
            let marks = vec!["?"; filter.statuses.len()].join(",");
            sql.push_str(&format!(" AND status IN ({marks})"));
            args.extend(filter.statuses.iter().cloned());
        }
        if let Some(app) = &filter.app {
            sql.push_str(" AND app = ?");
            args.push(app.clone());
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(filter.limit.clamp(1, 1000).to_string());
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_feedback)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn screenshots(&self, id: i64) -> Result<Vec<Screenshot>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT idx, mime, size FROM screenshots WHERE feedback_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([id], |row| {
            Ok(Screenshot {
                idx: row.get(0)?,
                mime: row.get(1)?,
                size: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn events(&self, id: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, at, author, kind, body FROM events WHERE feedback_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([id], |row| {
            Ok(Event {
                id: row.get(0)?,
                at: row.get(1)?,
                author: row.get(2)?,
                kind: row.get(3)?,
                body: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn add_event_tx(
        conn: &Connection,
        id: i64,
        author: &str,
        kind: &str,
        body: &str,
    ) -> Result<()> {
        let at = now();
        conn.execute(
            "INSERT INTO events (feedback_id, at, author, kind, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, at, author, kind, body],
        )?;
        conn.execute(
            "UPDATE feedback SET updated_at = ?2 WHERE id = ?1",
            params![id, at],
        )?;
        Ok(())
    }

    pub fn add_event(&self, id: i64, author: &str, kind: &str, body: &str) -> Result<()> {
        let conn = self.conn();
        Self::add_event_tx(&conn, id, author, kind, body)
    }

    /// Sets the status and records the change (plus an optional note) on the timeline.
    pub fn set_status(
        &self,
        id: i64,
        status: &str,
        author: &str,
        note: Option<&str>,
        agent: Option<&str>,
    ) -> Result<()> {
        if !STATUSES.contains(&status) {
            bail!(
                "unknown status {status:?}; expected one of {}",
                STATUSES.join(", ")
            );
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let previous: String =
            tx.query_row("SELECT status FROM feedback WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        tx.execute(
            "UPDATE feedback SET status = ?2, agent = COALESCE(?3, agent) WHERE id = ?1",
            params![id, status, agent],
        )?;
        let mut body = format!("{previous} → {status}");
        if let Some(agent) = agent {
            body.push_str(&format!(" (agent: {agent})"));
        }
        if let Some(note) = note.filter(|n| !n.trim().is_empty()) {
            body.push_str("\n\n");
            body.push_str(note.trim());
        }
        Self::add_event_tx(&tx, id, author, "status", &body)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let shots = self.screenshots(id)?;
        let deleted = self
            .conn()
            .execute("DELETE FROM feedback WHERE id = ?1", [id])?
            > 0;
        for shot in shots {
            let _ = std::fs::remove_file(self.screenshot_path(id, shot.idx, &shot.mime));
        }
        Ok(deleted)
    }

    /// Atomically hands the oldest queued item (optionally also never-attempted
    /// open items) whose requested agent this worker supports to the worker.
    pub fn claim(
        &self,
        worker: &str,
        agents: &[String],
        default_agent: &str,
        include_open: bool,
    ) -> Result<Option<Feedback>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let candidates: Vec<(i64, Option<String>, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, agent, status FROM feedback
                 WHERE status = 'queued' OR (?1 AND status = 'open' AND attempts = 0)
                 ORDER BY CASE status WHEN 'queued' THEN 0 ELSE 1 END, id",
            )?;
            let rows = stmt.query_map([include_open], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let pick = candidates.into_iter().find_map(|(id, agent, _)| {
            let agent = agent.unwrap_or_else(|| default_agent.to_string());
            agents.contains(&agent).then_some((id, agent))
        });
        let Some((id, agent)) = pick else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE feedback SET status = 'in_progress', agent = ?2, attempts = attempts + 1 WHERE id = ?1",
            params![id, agent],
        )?;
        Self::add_event_tx(
            &tx,
            id,
            worker,
            "claimed",
            &format!("Worker {worker} started agent {agent}"),
        )?;
        tx.commit()?;
        drop(conn);
        self.get(id)
    }
}
