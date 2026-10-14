mod agents;
mod apps;
mod client;
mod config;
mod db;
mod runs;
mod server;
mod worker;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use client::{
    Client, QueueOptions, default_author, find_app, print_list, print_run_events, print_runs,
    render_markdown, resolve_dir,
};
use config::ClientConfig;
use std::{path::PathBuf, sync::Arc};

/// Collect in-app feedback (with annotated screenshots) from all apps in one
/// place, and hand it to coding agents.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP server (widget, triage UI and API).
    Serve {
        #[arg(long, env = "FEEDBACK_HUB_BIND", default_value = "0.0.0.0:8080")]
        bind: String,
        #[arg(long, env = "FEEDBACK_HUB_DATA_DIR", default_value = "./data")]
        data_dir: PathBuf,
        #[arg(long, env = "FEEDBACK_HUB_STATIC_DIR")]
        static_dir: Option<PathBuf>,
        #[arg(long, env = "FEEDBACK_HUB_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: String,
        /// Comma-separated origin patterns allowed to submit, e.g. `https://*.example.org`.
        #[arg(
            long,
            env = "FEEDBACK_HUB_ALLOWED_ORIGINS",
            default_value = "http://localhost:*"
        )]
        allowed_origins: String,
        /// TOML file with `[[apps]]` entries (id, name, repo, checkout, hosts, notes).
        #[arg(long, env = "FEEDBACK_HUB_APPS_FILE")]
        apps_file: Option<PathBuf>,
        #[arg(long, env = "FEEDBACK_HUB_SQLITE_JOURNAL_MODE", default_value = "WAL")]
        journal_mode: String,
    },
    /// List feedback (default: everything not yet resolved or dismissed).
    List {
        #[arg(long, default_value = "open,queued,in_progress")]
        status: String,
        #[arg(long)]
        app: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: i64,
        #[arg(long)]
        json: bool,
    },
    /// Show one item as Markdown and download its screenshots.
    Show {
        id: i64,
        /// Where to save screenshots (default: ~/.cache/feedback-hub/<id>).
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print the prompt the worker would give an agent for this item.
    Prompt { id: i64 },
    /// Add a comment to an item's timeline.
    Comment {
        id: i64,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Change status: open, queued, in_progress, resolved, dismissed.
    Status {
        id: i64,
        status: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Queue an item for the agent worker.
    Queue {
        id: i64,
        /// claude, codex, hermes, … (default: the worker's default agent)
        #[arg(long)]
        agent: Option<String>,
        /// Model for the agent, e.g. `opus` or `gpt-6-sol` (default: the agent's own default).
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort, e.g. `low`, `high`, `xhigh`.
        #[arg(long)]
        effort: Option<String>,
        /// Extra instructions for the agent.
        #[arg(long)]
        note: Option<String>,
    },
    /// List agent runs (default: queued and running).
    Runs {
        #[arg(long)]
        all: bool,
        /// Only runs for this feedback item.
        #[arg(long)]
        feedback: Option<i64>,
    },
    /// Show a run's activity log.
    Run {
        id: i64,
        /// Keep printing new activity until the run ends.
        #[arg(long, short)]
        follow: bool,
    },
    /// Send a message to a running agent, or continue a finished run's session.
    Steer {
        run: i64,
        /// Interrupt the current turn instead of waiting for it to end (Codex, Hermes).
        #[arg(long)]
        interrupt: bool,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Stop a running agent or cancel a queued run.
    Stop { run: i64 },
    /// List registered apps and their repositories.
    Apps,
    /// Submit feedback from the command line (handy for testing).
    Submit {
        #[arg(long)]
        app: String,
        #[arg(long, default_value = "bug")]
        kind: String,
        message: Vec<String>,
    },
    /// Poll for queued items and run coding agents on them.
    Worker {
        /// Process at most one item, then exit.
        #[arg(long)]
        once: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve {
            bind,
            data_dir,
            static_dir,
            admin_token,
            allowed_origins,
            apps_file,
            journal_mode,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "feedback_hub=info".into()),
                )
                .init();
            anyhow::ensure!(
                admin_token.len() >= 16,
                "the admin token must be at least 16 characters"
            );
            let apps = match &apps_file {
                Some(path) => apps::parse(
                    &std::fs::read_to_string(path)
                        .with_context(|| format!("reading {}", path.display()))?,
                )?,
                None => Vec::new(),
            };
            let db = db::Database::open(&data_dir, &journal_mode)?;
            let server = server::Server::new(
                db,
                server::Config {
                    admin_token,
                    allowed_origins: allowed_origins
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    static_dir,
                    apps,
                },
            );
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!("listening on {bind}");
            axum::serve(listener, server::app(server))
                .with_graceful_shutdown(async {
                    let mut term =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("signal handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {},
                        _ = term.recv() => {},
                    }
                })
                .await?;
            Ok(())
        }
        // The remaining commands use the blocking client.
        command => tokio::task::spawn_blocking(move || run_client(command)).await?,
    }
}

fn run_client(command: Cmd) -> Result<()> {
    let config = ClientConfig::load()?;
    if let Cmd::Worker { once } = command {
        return Arc::new(worker::Worker::new(config)?).run(once);
    }
    let author = default_author();
    if let Cmd::Submit { app, kind, message } = command {
        let res =
            ureq::post(format!("{}/api/feedback", config.url())).send_json(serde_json::json!({
                "app": app,
                "kind": kind,
                "message": message.join(" "),
                "source": "cli",
                "reporter": author,
            }))?;
        println!("{}", res.into_body().read_to_string()?);
        return Ok(());
    }
    let client = Client::new(&config)?;
    match command {
        Cmd::List {
            status,
            app,
            limit,
            json,
        } => {
            let items = client.list(&status, app.as_deref(), limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                print_list(&items);
            }
        }
        Cmd::Show { id, dir, json } => {
            let detail = client.detail(id)?;
            let paths = client.download_screenshots(&detail, &resolve_dir(dir, id))?;
            if json {
                let mut value = serde_json::to_value(&detail.feedback)?;
                value["screenshot_paths"] = serde_json::to_value(&paths)?;
                value["events"] = serde_json::to_value(&detail.events)?;
                value["runs"] = serde_json::to_value(&detail.runs)?;
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let apps = client.apps().unwrap_or_default();
                print!(
                    "{}",
                    render_markdown(&detail, find_app(&apps, &detail.feedback.app), &paths)
                );
            }
        }
        Cmd::Prompt { id } => {
            let detail = client.detail(id)?;
            let apps = client.apps().unwrap_or_default();
            let paths = client.download_screenshots(&detail, &client::cache_dir(id))?;
            let workdir = PathBuf::from(format!("<worktree of {}>", detail.feedback.app));
            print!(
                "{}",
                worker::build_prompt(
                    &detail,
                    find_app(&apps, &detail.feedback.app),
                    &paths,
                    &worker::Target {
                        workdir: &workdir,
                        note: None,
                        branch: &format!("feedback/{id}"),
                        base: "main",
                        delivery: config.worker.delivery.as_deref().unwrap_or("push"),
                        hub_url: &config.url(),
                    },
                )
            );
        }
        Cmd::Comment { id, text } => {
            let kind = if std::env::var_os("FEEDBACK_HUB_ITEM").is_some() {
                "agent"
            } else {
                "comment"
            };
            client.comment(id, &author, kind, &text.join(" "))?;
            println!("Comment added to #{id}.");
        }
        Cmd::Status { id, status, note } => {
            client.set_status(id, &status, &author, note.as_deref())?;
            println!("#{id} is now {status}.");
        }
        Cmd::Queue {
            id,
            agent,
            model,
            effort,
            note,
        } => {
            let options = QueueOptions {
                agent,
                model,
                effort,
            };
            client.queue(id, &options, &author, note.as_deref())?;
            println!(
                "#{id} queued{}.",
                options
                    .agent
                    .map(|a| format!(" for {a}"))
                    .unwrap_or_default()
            );
        }
        Cmd::Runs { all, feedback } => print_runs(&client.runs(!all, feedback, 100)?),
        Cmd::Run { id, follow } => {
            let mut after = 0;
            loop {
                let detail = client.run(id, after)?;
                if after == 0 {
                    print_runs(std::slice::from_ref(&detail.run));
                }
                print_run_events(&detail.events);
                after = detail.events.last().map_or(after, |e| e.id);
                if !follow || !["queued", "running"].contains(&detail.run.status.as_str()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        Cmd::Steer {
            run,
            interrupt,
            text,
        } => {
            let run = client.command(run, "message", &text.join(" "), interrupt, &author)?;
            println!("Message sent to run {} ({}).", run.id, run.status);
        }
        Cmd::Stop { run } => {
            let run = client.command(run, "stop", "", false, &author)?;
            println!("Run {} is {}.", run.id, run.status);
        }
        Cmd::Apps => {
            for app in client.apps()? {
                println!(
                    "{:<16} {:<45} {}",
                    app.id,
                    app.repo.unwrap_or_default(),
                    app.hosts.join(", ")
                );
            }
        }
        Cmd::Serve { .. } | Cmd::Worker { .. } | Cmd::Submit { .. } => unreachable!(),
    }
    Ok(())
}
