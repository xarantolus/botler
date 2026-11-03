//! `botler`: the hub and its command line.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use botler_hub::auth::Principal;
use botler_hub::config::{HubConfig, Paths};
use botler_hub::{Hub, host, serve, template};
use botler_runtime::{Cli as Runtime, Containers};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "botler", version, about = "A personal system of autonomous agents.")]
struct Args {
    /// Data directory (database, monorepo, sessions, workspaces) [default: `data` in the config
    /// file, else ~/.local/share/botler].
    #[arg(long, env = "BOTLER_DATA", global = true)]
    data: Option<PathBuf>,
    /// Host configuration, optional (`data`, `listen`, `runtime`, `mirror`).
    #[arg(long, env = "BOTLER_CONFIG", default_value_os_t = host::config_dir().join("config.toml"), global = true)]
    config: PathBuf,
    /// Secret files, optional: each file named after a declared secret (e.g. `OPENROUTER_API_KEY`)
    /// sets it when the hub starts.
    #[arg(long, env = "BOTLER_SECRETS_DIR", default_value_os_t = host::config_dir().join("secrets"), global = true)]
    secrets_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the data directory and the monorepo from the built-in template (`serve` does this
    /// on its first start).
    Init,
    /// Run the hub.
    Serve {
        /// TCP address for the web app and API. Containers use the hub's Unix socket instead, so
        /// localhost is enough; reach it from other devices through a proxy (Tailscale serve)
        /// [default: `listen` in the config file, else 127.0.0.1:7420].
        #[arg(long, env = "BOTLER_LISTEN")]
        listen: Option<SocketAddr>,
        /// URL agents use to reach the hub (default: their forwarder, http://127.0.0.1:7420).
        #[arg(long, env = "BOTLER_CONTAINER_URL")]
        container_url: Option<String>,
        /// `podman` or `docker` (default: auto-detect).
        #[arg(long, env = "BOTLER_RUNTIME")]
        runtime: Option<String>,
    },
    /// Run the hub as a systemd user service (checks the host first).
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Print a user token for the web app and API.
    Token {
        #[arg(long, default_value = "90")]
        days: i64,
    },
    /// Print a one-time code that signs a browser in (valid 10 minutes).
    LoginCode,
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
    /// The monorepo template: which snapshot main builds on and whether an upgrade is pending.
    Template {
        #[command(subcommand)]
        command: TemplateCommand,
    },
    /// Write a consistent archive of the data directory (safe while the hub runs).
    Backup {
        /// Archive to create (zstd-compressed if `zstd` is installed, else gzip).
        dest: PathBuf,
        /// Leave out `secrets.key`; secret values in the archive then cannot be decrypted.
        #[arg(long)]
        no_key: bool,
        /// Also include files git ignores in the git checkouts of homes and workspaces
        /// (`node_modules`, `target`, `.venv`, ...), which are left out by default.
        #[arg(long)]
        full: bool,
    },
    /// Disk use by chat and by kind, largest first.
    Storage {
        /// `podman` or `docker` (default: auto-detect).
        #[arg(long, env = "BOTLER_RUNTIME")]
        runtime: Option<String>,
    },
    /// Remove unused images and exports, and run logs older than `--logs-older-than`.
    Gc {
        #[arg(long, default_value = "30d")]
        logs_older_than: botler_core::duration::HumanDuration,
        /// `podman` or `docker` (default: auto-detect).
        #[arg(long, env = "BOTLER_RUNTIME")]
        runtime: Option<String>,
    },
}

#[derive(Subcommand)]
enum TemplateCommand {
    /// The newest template snapshot, the template built into this hub, and pending upgrades.
    Status,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Only check whether this host has what the hub needs.
    Check,
    /// Check the host, write ~/.config/systemd/user/botler.service for this binary, enable and start it.
    Install,
    /// Stop and disable the service and remove its unit (the data directory stays).
    Uninstall,
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
        // Logs go to stderr: stdout is for command output (`botler token > file`).
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let host = host::HostConfig::load(&args.config)?;
    let data = args.data.or_else(|| host.data_dir()).unwrap_or_else(default_data_dir);
    // Absolute, so nothing depends on the working directory of the git or podman it runs.
    let paths = Paths::new(std::path::absolute(&data).context("resolving the data directory")?);
    match args.command {
        Command::Init => init(&paths, &host).await,
        Command::Serve { listen, container_url, runtime } => {
            let listen = listen.or(host.listen).unwrap_or(SocketAddr::from(([127, 0, 0, 1], 7420)));
            if !paths.repo().exists() {
                let commit = host::init(&paths, host.mirror.as_deref()).await?;
                tracing::info!(data = %paths.root.display(), %commit, "created the data directory and the monorepo");
            }
            let mut config = HubConfig::new(listen);
            if let Some(u) = container_url {
                config.container_url = u;
            }
            let rt = Runtime::detect(runtime.as_deref().or(host.runtime.as_deref()))
                .await
                .context("no container runtime")?;
            tracing::info!(runtime = ?rt.info(), "container runtime");
            config.runtime = Some(rt.info().binary.clone());
            let hub = Hub::open(paths, config, Arc::new(rt)).await.context("opening the hub")?;
            for name in host::import_secrets(&hub, &args.secrets_dir).await? {
                tracing::info!(secret = name, dir = %args.secrets_dir.display(), "secret set from its file");
            }
            if hub.store.devices().await?.is_empty() {
                // Nothing signed in yet: a link that signs this browser in.
                let (code, _) = botler_hub::login::create_code(&hub).await?;
                let at = if listen.ip().is_unspecified() {
                    SocketAddr::from(([127, 0, 0, 1], listen.port()))
                } else {
                    listen
                };
                tracing::info!(
                    "no device is signed in yet: open http://{at}/#code={code} (valid {} minutes; \
                     `botler login-code` prints a new code)",
                    botler_hub::login::CODE_MINUTES
                );
            }
            serve::serve(hub, shutdown_signal()).await?;
            Ok(())
        }
        Command::Service { command } => service(command, host.runtime.as_deref()),
        Command::Token { days } => {
            let hub = offline_hub(paths).await?;
            println!("{}", hub.tokens.issue(&Principal::User, chrono::Duration::days(days), chrono::Utc::now()));
            Ok(())
        }
        Command::LoginCode => {
            let hub = offline_hub(paths).await?;
            let (code, _) = botler_hub::login::create_code(&hub).await?;
            println!("{code}");
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
        Command::Template { command: TemplateCommand::Status } => template_status(paths).await,
        Command::Backup { dest, no_key, full } => backup(&paths, &dest, !no_key, full).await,
        Command::Storage { runtime } => storage(runtime_hub(paths, runtime).await?).await,
        Command::Gc { logs_older_than, runtime } => {
            let hub = runtime_hub(paths, runtime).await?;
            let freed = botler_hub::storage::collect_garbage(&hub, logs_older_than.0).await?;
            println!("freed {}", human_bytes(freed));
            Ok(())
        }
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

async fn init(paths: &Paths, host: &host::HostConfig) -> anyhow::Result<()> {
    let commit = host::init(paths, host.mirror.as_deref()).await?;
    offline_hub(paths.clone()).await?; // creates the database
    println!("botler initialised in {} (monorepo at {commit})", paths.root.display());
    println!("Start the hub with `botler serve`; it prints a link that signs your browser in.");
    Ok(())
}

/// Print what the host lacks; an error when something blocks the hub.
fn check(runtime: Option<&str>) -> anyhow::Result<()> {
    let user = std::env::var("USER").context("USER is not set")?;
    let problems = host::preflight(&user, runtime);
    for p in &problems {
        println!("{} {}\n    fix: {}", if p.blocking { "missing:" } else { "note:" }, p.what, p.fix);
    }
    if problems.iter().any(|p| p.blocking) {
        bail!("fix the above, then run this again");
    }
    Ok(())
}

fn service(command: ServiceCommand, runtime: Option<&str>) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let unit = PathBuf::from(home).join(".config/systemd/user/botler.service");
    let systemctl = |args: &[&str]| -> anyhow::Result<()> {
        let status =
            std::process::Command::new("systemctl").arg("--user").args(args).status().context("running systemctl")?;
        if !status.success() {
            bail!("systemctl --user {} failed ({status})", args.join(" "));
        }
        Ok(())
    };
    match command {
        ServiceCommand::Check => {
            check(runtime)?;
            println!("this host has what botler needs");
            Ok(())
        }
        ServiceCommand::Install => {
            check(runtime)?;
            let exe = std::env::current_exe().context("finding this binary")?;
            std::fs::create_dir_all(unit.parent().context("unit path")?)?;
            std::fs::write(&unit, host::systemd_unit(&exe))?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "--now", "botler"])?;
            println!("wrote {} and started it.", unit.display());
            println!("The first start builds the agent image (minutes); the sign-in link is in its log:");
            println!("  journalctl --user -u botler -f");
            Ok(())
        }
        ServiceCommand::Uninstall => {
            if unit.exists() {
                systemctl(&["disable", "--now", "botler"])?;
                std::fs::remove_file(&unit)?;
                systemctl(&["daemon-reload"])?;
            }
            println!("botler's service is removed; the data directory is untouched.");
            Ok(())
        }
    }
}

async fn validate(paths: &Paths, dir: &std::path::Path) -> anyhow::Result<()> {
    let report = botler_core::load_with(&botler_core::defs::FsTree::new(dir), botler_hub::localtime::host());
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
    let tz_from = if applied.defs.instance.timezone.is_some() { "botler.toml" } else { "host" };
    println!("timezone   {} (from {tz_from})", applied.defs.timezone.name());
    for i in hub.store.live_instances().await? {
        println!("instance   {:<28} {:<9} {}", i.name, i.state, i.task.as_deref().unwrap_or(""));
    }
    for d in hub.store.open_dialogs().await? {
        println!("dialog     {} ({}): {}", d.id, d.instance, d.question);
    }
    for inc in hub.store.open_incidents().await? {
        println!("incident   {} {} x{}", inc.runnable, inc.kind, inc.count);
    }
    for s in botler_hub::services::status(&hub).await? {
        let urls: Vec<&str> = s.published.iter().map(|p| p.url.as_str()).collect();
        let state = format!("{:?}", s.state).to_lowercase();
        println!("service    {:<28} {state:<9} {}", s.name, urls.join(" "));
    }
    for b in hub.store.breakers().await? {
        println!("breaker    {} ({}): {}", b.subject, b.kind, b.reason);
    }
    Ok(())
}

async fn template_status(paths: Paths) -> anyhow::Result<()> {
    let hub = offline_hub(paths).await?;
    let embedded = template::embedded_tree(&hub.repo, &hub.paths.worktrees()).await?;
    println!("embedded   hub v{} (tree {})", env!("CARGO_PKG_VERSION"), &embedded[..12]);
    let Some(tip) = template::snapshot(&hub.repo).await? else {
        println!("snapshot   none: main's root commit is not a botler template, so upgrades are off");
        return Ok(());
    };
    let tree = hub.repo.tree(&tip).await?;
    let main = hub.repo.main().await?;
    let merged = if hub.repo.is_ancestor(&tip, &main).await? { "merged into main" } else { "not on main yet" };
    println!("snapshot   {} (tree {}, {merged})", &tip[..12], &tree[..12]);
    if tree != embedded {
        println!("upgrade    the next `botler serve` proposes the embedded template");
    }
    for p in template::pending(&hub).await? {
        println!("proposal   {} {} on {}: {}", p.id, p.status, p.branch.as_deref().unwrap_or(""), p.title);
    }
    Ok(())
}

/// Data directory entries that are facts. Everything else (exports, worktrees, logs, agent-dirs,
/// assets, images, the search index `search.db`) is derived and rebuilt by the hub.
const BACKUP_DIRS: &[&str] =
    &["repo.git", "memory", "sessions", "workspaces", "homes", "shared-homes", "uploads", "inbox"];

async fn backup(paths: &Paths, dest: &std::path::Path, with_key: bool, full: bool) -> anyhow::Result<()> {
    if !paths.db().exists() {
        bail!("{} is not initialised; run `botler init`", paths.root.display());
    }
    // Created up front: refuses to overwrite, and is private from the start (it may hold the key).
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let file = opts.open(dest).with_context(|| format!("creating {}", dest.display()))?;
    let staging = dest.with_extension("staging");
    let result = write_backup(paths, file, &staging, with_key, full).await;
    let cleanup = match std::fs::remove_dir_all(&staging) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if let Err(e) = result {
        for (what, r) in [("staging dir", cleanup), ("incomplete archive", std::fs::remove_file(dest))] {
            if let Err(c) = r {
                eprintln!("warning: could not remove the {what}: {c}");
            }
        }
        return Err(e);
    }
    cleanup.with_context(|| format!("removing {}", staging.display()))?;
    println!("backup written to {}", dest.display());
    if with_key {
        eprintln!(
            "warning: the archive holds secrets.key and hub.db; together they decrypt every secret. Keep it private."
        );
    }
    Ok(())
}

async fn write_backup(
    paths: &Paths,
    file: std::fs::File,
    staging: &std::path::Path,
    with_key: bool,
    full: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir(staging).with_context(|| format!("creating {}", staging.display()))?;
    // What git ignores in the checkouts of homes and workspaces is rebuildable. tar's own
    // `--exclude-vcs-ignores` gets negations and anchoring wrong, so git lists it; names with a
    // newline cannot be written as a pattern and stay in.
    let mut excludes = String::new();
    if !full {
        for dir in ["homes", "shared-homes", "workspaces"] {
            for p in botler_hub::storage::ignored_paths(&paths.root.join(dir))? {
                let rel = p.strip_prefix(&paths.root)?.to_string_lossy().into_owned();
                if !rel.contains('\n') {
                    excludes.push_str(&rel);
                    excludes.push('\n');
                }
            }
        }
    }
    let exclude_file = std::path::absolute(staging.join("excludes"))?;
    std::fs::write(&exclude_file, excludes)?;
    let store = botler_store::Store::open(&paths.db()).await?;
    store.backup(&staging.join("hub.db")).await.context("snapshotting hub.db")?;
    let zstd = tokio::process::Command::new("zstd").arg("--version").output().await.is_ok_and(|o| o.status.success());
    let mut tar = tokio::process::Command::new("tar");
    tar.arg("-c").arg(if zstd { "--zstd" } else { "--gzip" }).arg("-f").arg("-");
    tar.args(["--no-wildcards", "--anchored"]).arg("--exclude-from").arg(&exclude_file);
    // Absolute, because each `-C` is relative to the previous one.
    tar.arg("-C").arg(std::path::absolute(staging)?).arg("hub.db").arg("-C").arg(std::path::absolute(&paths.root)?);
    tar.args(BACKUP_DIRS.iter().filter(|d| paths.root.join(d).exists()));
    if with_key {
        tar.arg("secrets.key");
    }
    let status = tar.stdout(file).status().await.context("running tar")?;
    // GNU tar exits 1 when a file changed while being read (a live session log); the archive is
    // still complete, and tar has printed which file.
    match status.code() {
        Some(0) => Ok(()),
        Some(1) => {
            eprintln!("warning: some files changed while being archived (see above)");
            Ok(())
        }
        _ => bail!("tar failed ({status})"),
    }
}

/// A hub handle with the real container runtime (no containers started), for commands that look
/// at images: with a fake runtime every image and export would look unused.
async fn runtime_hub(paths: Paths, runtime: Option<String>) -> anyhow::Result<Arc<Hub>> {
    if !paths.repo().exists() {
        bail!("{} is not initialised; run `botler init`", paths.root.display());
    }
    let rt = Runtime::detect(runtime.as_deref()).await.context("no container runtime")?;
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    Ok(Hub::open(paths, config, Arc::new(rt)).await?)
}

async fn storage(hub: Arc<Hub>) -> anyhow::Result<()> {
    let o = botler_hub::storage::overview(&hub, true).await?;
    println!("total      {}", human_bytes(o.total_bytes));
    for c in &o.chats {
        let packed = if c.packed { " (packed)" } else { "" };
        println!(
            "chat       {:<28} {:<9} {:>10}  home {}{packed}, workspace {}, session {}",
            c.instance,
            c.state,
            human_bytes(c.total_bytes),
            human_bytes(c.home_bytes),
            human_bytes(c.workspace_bytes),
            human_bytes(c.session_bytes)
        );
    }
    for i in &o.items {
        let kind = serde_json::to_value(i.kind)?;
        println!("{:<10} {:<28} {:>10}", kind.as_str().unwrap_or_default(), i.name, human_bytes(i.bytes));
    }
    Ok(())
}

fn human_bytes(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if v < 1024.0 {
            return format!("{v:.1} {unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.1} TiB")
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
