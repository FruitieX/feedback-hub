mod apps;
mod client;
mod config;
mod db;
mod server;
mod worker;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use client::{Client, default_author, find_app, print_list, render_markdown, resolve_dir};
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
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
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
            let server = server::Server {
                db,
                config: Arc::new(server::Config {
                    admin_token,
                    allowed_origins: allowed_origins
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    static_dir,
                    apps,
                }),
            };
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
        return worker::Worker::new(config)?.run(once);
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
        Cmd::Queue { id, agent, note } => {
            client.queue(id, agent.as_deref(), &author, note.as_deref())?;
            println!(
                "#{id} queued{}.",
                agent.map(|a| format!(" for {a}")).unwrap_or_default()
            );
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
