//! `botler`: the hub and its command line.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use botler_hub::auth::Principal;
use botler_hub::config::{HubConfig, Paths};
use botler_hub::{Hub, serve, template};
use botler_runtime::{Cli as Runtime, Containers};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "botler", version, about = "A personal system of autonomous agents.")]
struct Args {
    /// Data directory (database, monorepo, sessions, workspaces).
    #[arg(long, env = "BOTLER_DATA", default_value_os_t = default_data_dir(), global = true)]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the data directory and the monorepo from the built-in template.
    Init,
    /// Run the hub.
    Serve {
        #[arg(long, env = "BOTLER_LISTEN", default_value = "0.0.0.0:7420")]
        listen: SocketAddr,
        /// URL containers use to reach the hub (default: host.containers.internal:<port>).
        #[arg(long, env = "BOTLER_CONTAINER_URL")]
        container_url: Option<String>,
        /// `podman` or `docker` (default: auto-detect).
        #[arg(long, env = "BOTLER_RUNTIME")]
        runtime: Option<String>,
    },
    /// Print a user token for the web app and API.
    Token {
        #[arg(long, default_value = "90")]
        days: i64,
    },
    /// Validate a monorepo checkout and print its deployment summary against main.
    Validate {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Manage secret values.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Stop all agents and freeze scheduling (kill switch).
    Pause,
    /// Undo `pause`.
    Resume,
    /// Show what the hub is doing.
    Status,
}

#[derive(Subcommand)]
enum SecretCommand {
    /// Set a declared secret; the value is read from stdin.
    Set { name: String },
    /// List declared secrets and whether they have values.
    List,
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("botler")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,botler=info".into()),
        )
        .init();
    let args = Args::parse();
    let paths = Paths::new(&args.data);
    match args.command {
        Command::Init => init(&paths).await,
        Command::Serve { listen, container_url, runtime } => {
            let mut config = HubConfig::new(listen);
            if let Some(u) = container_url {
                config.container_url = u;
            }
            let rt = Runtime::detect(runtime.as_deref()).await.context("no container runtime")?;
            tracing::info!(runtime = ?rt.info(), "container runtime");
            config.runtime = Some(rt.info().binary.clone());
            let hub = Hub::open(paths, config, Arc::new(rt)).await.context("opening the hub")?;
            serve::serve(hub, shutdown_signal()).await?;
            Ok(())
        }
        Command::Token { days } => {
            let hub = offline_hub(paths).await?;
            println!("{}", hub.tokens.issue(&Principal::User, chrono::Duration::days(days), chrono::Utc::now()));
            Ok(())
        }
        Command::Validate { dir } => validate(&paths, &dir).await,
        Command::Secret { command } => {
            let hub = offline_hub(paths).await?;
            match command {
                SecretCommand::Set { name } => {
                    if !hub.applied().await.defs.secrets.contains_key(&name) {
                        bail!("secret {name} is not declared in the monorepo's secrets.toml");
                    }
                    let mut value = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut value)?;
                    let value = value.trim_end_matches(['\n', '\r']);
                    if value.is_empty() {
                        bail!("empty value on stdin");
                    }
                    hub.store.set_secret(&hub.secret_key, &name, value).await?;
                    println!("{name} set");
                }
                SecretCommand::List => {
                    let set = hub.store.secret_names().await?;
                    let applied = hub.applied().await;
                    for g in applied.defs.secret_groups.values() {
                        println!("{} ({}): {}", g.name, format!("{:?}", g.class).to_lowercase(), g.description);
                        for s in g.secrets.iter().filter_map(|n| applied.defs.secrets.get(n)) {
                            let mark = if set.contains(&s.name) { "set" } else { "MISSING" };
                            let how = s.file.as_deref().map_or_else(|| "env".to_string(), |f| format!("file {f}"));
                            println!("  {:<28} {:<8} {:<24} {}", s.name, mark, how, s.description);
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Pause => {
            // Takes effect in a running hub within seconds (it re-reads the flag every loop).
            offline_hub(paths).await?.store.set_paused(true).await?;
            println!("paused");
            Ok(())
        }
        Command::Resume => {
            offline_hub(paths).await?.store.set_paused(false).await?;
            println!("resumed");
            Ok(())
        }
        Command::Status => status(paths).await,
    }
}

/// A hub handle for CLI commands that only touch the store and repo (no containers started).
async fn offline_hub(paths: Paths) -> anyhow::Result<Arc<Hub>> {
    if !paths.repo().exists() {
        bail!("{} is not initialised; run `botler init`", paths.root.display());
    }
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    Ok(Hub::open(paths, config, Arc::new(botler_runtime::fake::Fake::new())).await?)
}

async fn init(paths: &Paths) -> anyhow::Result<()> {
    if paths.repo().exists() {
        bail!("{} already holds a botler monorepo", paths.root.display());
    }
    paths.ensure()?;
    let tmp = paths.root.join("template-init");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    template::write_to(&tmp)?;
    let commit = botler_repo::Repo::new(paths.repo(), paths.scratch()).init_from(&tmp).await?;
    std::fs::remove_dir_all(&tmp)?;
    let hub = offline_hub(paths.clone()).await?;
    let token = hub.tokens.issue(&Principal::User, chrono::Duration::days(90), chrono::Utc::now());
    println!("botler initialised in {} (monorepo at {commit})", paths.root.display());
    println!();
    println!("Next:");
    println!("  1. Enter your OpenRouter key:   botler secret set OPENROUTER_API_KEY");
    println!("  2. Start the hub:               botler serve");
    println!("  3. Your user token (90 days):   {token}");
    Ok(())
}

async fn validate(paths: &Paths, dir: &std::path::Path) -> anyhow::Result<()> {
    let report = botler_core::load(&botler_core::defs::FsTree::new(dir));
    for d in &report.diagnostics {
        println!("{d}");
    }
    let Some(new) = report.definitions else { bail!("definitions are invalid") };
    if paths.repo().exists() {
        let hub = offline_hub(paths.clone()).await?;
        let applied = hub.applied().await;
        let summary = botler_core::change::preview(&applied.defs, &new, &[], chrono::Utc::now());
        println!("{}", summary.to_markdown(new.timezone));
    } else {
        println!("definitions are valid (no hub data at {} to compare against)", paths.root.display());
    }
    Ok(())
}

async fn status(paths: Paths) -> anyhow::Result<()> {
    let hub = offline_hub(paths).await?;
    let applied = hub.applied().await;
    println!("commit     {}", applied.commit);
    println!("paused     {}", hub.store.paused().await?);
    for i in hub.store.live_instances().await? {
        println!("instance   {:<28} {:<9} {}", i.name, i.state, i.task.as_deref().unwrap_or(""));
    }
    for d in hub.store.open_dialogs().await? {
        println!("dialog     {} ({}): {}", d.id, d.instance, d.question);
    }
    for inc in hub.store.open_incidents().await? {
        println!("incident   {} {} x{}", inc.runnable, inc.kind, inc.count);
    }
    for b in hub.store.breakers().await? {
        println!("breaker    {} ({}): {}", b.subject, b.kind, b.reason);
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
