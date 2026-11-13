use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use orqella::config::{Config, Paths};
use orqella::{backup, config, http, modules, util};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "orqella",
    version,
    about = "A local-first, modular dashboard platform"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the local daemon.
    Serve {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        no_dashboard: bool,
        /// Enable the browser WebRTC bridge for this process.
        #[arg(long)]
        web: bool,
    },
    /// Print and optionally open the dashboard URL.
    Open {
        #[arg(long)]
        no_open: bool,
    },
    /// Print, install, or remove local hostname entries (needed by Safari).
    #[cfg(unix)]
    Hosts {
        #[command(subcommand)]
        command: HostsCommand,
    },
    /// Module authoring and installation.
    Module {
        #[command(subcommand)]
        command: ModuleCommand,
    },
    /// Start an AI module build against the running daemon.
    Build { prompt: String },
    /// Install or remove the per-user background service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Check for or apply daemon updates.
    Update {
        #[command(subcommand)]
        command: UpdateCommand,
    },
    /// Check the local installation.
    Doctor,
    /// Remove the background service and optionally local Orqella state.
    Uninstall {
        /// Also delete config, module data, and daemon data.
        #[arg(long)]
        purge: bool,
        /// Do not prompt for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Write a local backup zip (includes config and modules).
    Backup { output: Option<PathBuf> },
    /// Write a redacted support bundle for troubleshooting.
    Support { output: Option<PathBuf> },
}

#[derive(Subcommand)]
#[cfg(unix)]
enum HostsCommand {
    /// Print the managed /etc/hosts block.
    Print,
    /// Add or refresh Orqella hostnames in /etc/hosts.
    Install,
    /// Remove the Orqella-managed hostnames.
    Remove,
}

#[derive(Subcommand)]
enum ModuleCommand {
    /// Scaffold a new module.
    New {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Validate a module directory.
    Check { path: PathBuf },
    /// Install a module directory or git repository URL.
    Install {
        source: String,
        #[arg(long)]
        branch: Option<String>,
    },
    /// List installed modules.
    List,
    /// Print the module agent guide.
    Guide,
    /// Install a module into the running daemon and print its page URL.
    Dev { path: PathBuf },
    /// Update an installed module from its git upstream.
    Update { id: String },
    /// Commit the current module tree to its own git repository.
    Checkpoint { id: String, message: Option<String> },
    /// Publish an installed module to GitHub.
    Publish {
        id: String,
        #[arg(long)]
        repo: Option<String>,
        /// Publish a public repository instead of the default private one.
        #[arg(long)]
        public: bool,
        /// Fork the module's existing GitHub remote, then publish the fork.
        #[arg(long)]
        fork: bool,
        #[arg(long)]
        message: Option<String>,
    },
    /// Remove an installed module.
    Remove { id: String },
    /// Export a module package to a file.
    Export { id: String, output: PathBuf },
    /// Import a module package file.
    Import { file: PathBuf },
    /// Push an installed module to a machine connected through Link.
    Push { id: String, peer: String },
    /// Print this daemon's module signing public key and fingerprint.
    Key,
    /// Trust a publisher public key (hex).
    Trust {
        public_key: String,
        #[arg(long, default_value = "publisher")]
        label: String,
    },
    /// Sign a module directory in place.
    Sign { path: PathBuf },
    /// Verify a module package file.
    Verify { file: PathBuf },
}

#[derive(Subcommand)]
enum ServiceCommand {
    Install,
    Uninstall,
    Status,
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Compare the configured checkout with its upstream branch.
    Check,
    /// Remember the git checkout used for future updates.
    Configure { repo: PathBuf },
    /// Pull, rebuild, install, and restart the daemon.
    Apply {
        #[arg(long)]
        parent_pid: Option<u32>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("orqella=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    let paths = Paths::discover()?;
    let mut config = Config::load_or_create(&paths)?;
    if let Ok(command) = std::env::var("ORQELLA_AGENT_COMMAND")
        && !command.trim().is_empty()
    {
        config.agent.command = command;
    }
    match cli.command.unwrap_or(Command::Serve {
        port: None,
        no_dashboard: false,
        web: false,
    }) {
        Command::Serve {
            port,
            no_dashboard,
            web,
        } => {
            if web {
                config.web.enabled = true;
            }
            http::serve(config, paths, port, no_dashboard).await?;
        }
        Command::Open { no_open } => {
            let url = config.origin();
            println!("{url}");
            if !no_open {
                open_url(&url);
            }
        }
        #[cfg(unix)]
        Command::Hosts { command } => hosts_command(command, &config)?,
        Command::Module { command } => module_command(command, paths, config).await?,
        Command::Build { prompt } => build_command(prompt, config, paths.clone()).await?,
        Command::Service { command } => service_command(command, &config)?,
        Command::Update { command } => match command {
            UpdateCommand::Check => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&orqella::update::check(&config)?)?
                );
            }
            UpdateCommand::Configure { repo } => {
                let repo = repo.canonicalize().unwrap_or(repo);
                config.update.repo = repo.to_string_lossy().to_string();
                config.save(&paths)?;
                println!("update repo set to {}", repo.display());
            }
            UpdateCommand::Apply { parent_pid } => {
                orqella::update::apply(&config, &paths, parent_pid)?;
            }
        },
        Command::Doctor => doctor(paths, config).await?,
        Command::Uninstall { purge, yes } => uninstall(paths, &config, purge, yes)?,
        Command::Backup { output } => {
            let destination = output.unwrap_or_else(|| default_archive_path("backup"));
            let written = backup::create_backup(&paths, &destination)?;
            println!("backup written to {}", written.display());
        }
        Command::Support { output } => {
            let destination = output.unwrap_or_else(|| default_archive_path("support"));
            let written = backup::create_support_bundle(&paths, &config, &destination)?;
            println!("support bundle written to {}", written.display());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn hosts_command(command: HostsCommand, config: &Config) -> Result<()> {
    match command {
        HostsCommand::Print => print!("{}", orqella::hosts::block(config)),
        HostsCommand::Install => orqella::hosts::install(config)?,
        HostsCommand::Remove => orqella::hosts::remove(config)?,
    }
    Ok(())
}

fn admin_header(paths: &Paths) -> String {
    std::fs::read_to_string(paths.data_dir.join("admin.token"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

async fn module_command(command: ModuleCommand, paths: Paths, config: Config) -> Result<()> {
    match command {
        ModuleCommand::New { path, force } => scaffold_module(&path, force)?,
        ModuleCommand::Check { path } => check_module(&path)?,
        ModuleCommand::Install { source, branch } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/install", config.origin());
            let body = if modules::git::is_remote(&source) {
                serde_json::json!({ "url": source, "branch": branch.clone().unwrap_or_default() })
            } else {
                serde_json::json!({ "path": source })
            };
            let response = client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .json(&body)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let body: serde_json::Value = response.json().await.unwrap_or_default();
                    let module = &body["module"];
                    println!(
                        "installed {} {} ({})",
                        module["name"].as_str().unwrap_or("module"),
                        module["version"].as_str().unwrap_or(""),
                        module["id"].as_str().unwrap_or("")
                    );
                    if let Some(error) = body["sidecar_error"].as_str() {
                        eprintln!("setup warning: {error}");
                    }
                }
                Ok(response) => bail!(
                    "install failed: {} {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                ),
                Err(_) => {
                    let store = modules::ModuleStore::load(paths).await?;
                    let installed = if modules::git::is_remote(&source) {
                        store.install_from_source(&source, branch.as_deref())?
                    } else {
                        store.install_from_path(Path::new(&source))?
                    };
                    println!(
                        "installed {} {} ({}); start the daemon to use it",
                        installed.manifest.name, installed.manifest.version, installed.manifest.id
                    );
                }
            }
        }
        ModuleCommand::Update { id } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/{}/update", config.origin(), id);
            match client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let body: serde_json::Value = response.json().await.unwrap_or_default();
                    let module = &body["module"];
                    println!(
                        "updated {} {} ({})",
                        module["name"].as_str().unwrap_or("module"),
                        module["version"].as_str().unwrap_or(""),
                        module["id"].as_str().unwrap_or(&id)
                    );
                }
                Ok(response) => bail!(
                    "update failed: {} {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                ),
                Err(_) => {
                    let store = modules::ModuleStore::load(paths).await?;
                    let updated = store.update_from_git(&id)?;
                    println!(
                        "updated {} {} ({}); start the daemon to use it",
                        updated.manifest.name, updated.manifest.version, updated.manifest.id
                    );
                }
            }
        }
        ModuleCommand::Checkpoint { id, message } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/{}/checkpoint", config.origin(), id);
            let payload = serde_json::json!({ "message": message.unwrap_or_default() });
            match client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .json(&payload)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => println!("checkpointed {id}"),
                Ok(response) => bail!(
                    "checkpoint failed: {} {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                ),
                Err(_) => {
                    let store = modules::ModuleStore::load(paths).await?;
                    store.checkpoint(&id, "Checkpoint from orqella CLI")?;
                    println!("checkpointed {id}; start the daemon to use it");
                }
            }
        }
        ModuleCommand::Publish {
            id,
            repo,
            public,
            fork,
            message,
        } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/github/publish", config.origin());
            let repository = repo.unwrap_or_default();
            let confirm_repository = repository.clone();
            let response = client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .json(&serde_json::json!({
                    "module_id": id,
                    "repository": repository,
                    "confirm_repository": confirm_repository,
                    "private": !public,
                    "fork": fork,
                    "message": message.unwrap_or_default(),
                }))
                .send()
                .await
                .context("the Orqella daemon must be running to publish a module")?;
            if !response.status().is_success() {
                bail!(
                    "publish failed: {} {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                );
            }
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            println!(
                "published {} to {} ({})",
                id,
                body["repository"].as_str().unwrap_or("GitHub"),
                body["branch"].as_str().unwrap_or("main")
            );
            if let Some(url) = body["url"].as_str() {
                println!("{url}");
            }
        }
        ModuleCommand::List => {
            let store = modules::ModuleStore::load(paths).await?;
            for module in store.list() {
                println!(
                    "{:<24} {:<12} {:<8} {}",
                    module.manifest.id,
                    module.manifest.version,
                    if module.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    module.manifest.name
                );
            }
        }
        ModuleCommand::Guide => {
            println!("{}", include_str!("../docs/MODULE_AGENT.md"));
        }
        ModuleCommand::Dev { path } => {
            let manifest = modules::Manifest::load(&path)?;
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/install", config.origin());
            let response = client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .json(&serde_json::json!({ "path": path }))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!(
                    "could not install module: {}",
                    response.text().await.unwrap_or_default()
                );
            }
            let page = format!("{}/m/{}/", config.origin(), manifest.id);
            println!("{page}");
            open_url(&page);
        }
        ModuleCommand::Remove { id } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/{}", config.origin(), id);
            let response = client
                .delete(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!(
                    "remove failed: {}",
                    response.text().await.unwrap_or_default()
                );
            }
            println!("removed {id}");
        }
        ModuleCommand::Export { id, output } => {
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/{}/export", config.origin(), id);
            let response = client
                .get(url)
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!(
                    "export failed: {}",
                    response.text().await.unwrap_or_default()
                );
            }
            let bytes = response.bytes().await?;
            std::fs::write(&output, bytes)?;
            println!("exported {id} to {}", output.display());
        }
        ModuleCommand::Import { file } => {
            let bytes = std::fs::read(&file)?;
            let client = reqwest::Client::new();
            let url = format!("{}/api/v1/modules/import", config.origin());
            let response = client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .header("Content-Type", "application/zip")
                .body(bytes)
                .send()
                .await?;
            if !response.status().is_success() {
                bail!(
                    "import failed: {}",
                    response.text().await.unwrap_or_default()
                );
            }
            println!("imported {}", file.display());
        }
        ModuleCommand::Push { id, peer } => {
            let client = reqwest::Client::new();
            let url = format!(
                "{}/api/v1/peers/{}/modules/{}/sync",
                config.origin(),
                peer,
                id
            );
            let response = client
                .post(url)
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", admin_header(&paths))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!("push failed: {}", response.text().await.unwrap_or_default());
            }
            println!("pushed {id} to {peer}");
        }
        ModuleCommand::Key => {
            let key = modules::signing::load_or_create_key(&paths)?;
            let public = modules::signing::public_key_hex(&key);
            println!("public-key {public}");
            println!("fingerprint {}", modules::signing::fingerprint(&public));
        }
        ModuleCommand::Trust { public_key, label } => {
            if public_key.len() != 64 || hex::decode(&public_key).is_err() {
                bail!("public key must be 32 bytes of hex");
            }
            let token = admin_header(&paths);
            let client = reqwest::Client::new();
            let response = client
                .post(format!("{}/api/v1/trusted-keys", config.origin()))
                .header("X-Orqella", "1")
                .header("X-Orqella-Admin-Token", token)
                .json(&serde_json::json!({ "public_key": public_key, "label": label }))
                .send()
                .await;
            if let Ok(response) = response {
                if response.status().is_success() {
                    println!("trusted {public_key}");
                    return Ok(());
                }
                if !matches!(
                    response.status(),
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::NOT_FOUND
                ) {
                    bail!(
                        "trust failed: {}",
                        response.text().await.unwrap_or_default()
                    );
                }
            }
            let mut config = Config::load_or_create(&paths)?;
            if !config
                .trusted_module_keys
                .iter()
                .any(|key| key.public_key.eq_ignore_ascii_case(&public_key))
            {
                config
                    .trusted_module_keys
                    .push(config::TrustedKey { public_key, label });
                config.save(&paths)?;
            }
            println!("trusted key saved");
        }
        ModuleCommand::Sign { path } => {
            let key = modules::signing::load_or_create_key(&paths)?;
            let record = modules::signing::sign_tree(&path, &key)?;
            println!("signed {} ({})", path.display(), record.fingerprint);
        }
        ModuleCommand::Verify { file } => {
            let bytes = std::fs::read(&file)?;
            let temp = modules::package::import(&bytes)?;
            let report = modules::signing::verify_tree(
                temp.path(),
                &config.trusted_module_keys,
                false,
                false,
            )?;
            println!(
                "verified: signed={} trusted={} fingerprint={}",
                report.signed,
                report.trusted,
                report.fingerprint.unwrap_or_default()
            );
        }
    }
    Ok(())
}

async fn build_command(prompt: String, config: Config, paths: Paths) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/build", config.origin());
    let response = client
        .post(&url)
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", admin_header(&paths))
        .json(&serde_json::json!({ "prompt": prompt }))
        .send()
        .await
        .with_context(|| format!("could not reach the daemon at {url}"))?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    if !status.is_success() {
        bail!("build request failed: {body}");
    }
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

fn service_command(command: ServiceCommand, config: &Config) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let _ = config;
        linux_service(command)
    }
    #[cfg(target_os = "macos")]
    {
        macos_service(command, config)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = config;
        windows_service(command)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (command, config);
        bail!("service installation is not implemented for this platform; run `orqella serve`")
    }
}

#[cfg(target_os = "linux")]
fn linux_service(command: ServiceCommand) -> Result<()> {
    let unit_dir = dirs::config_dir()
        .context("config directory unavailable")?
        .join("systemd/user");
    let unit = unit_dir.join("orqella.service");
    match command {
        ServiceCommand::Install => {
            std::fs::create_dir_all(&unit_dir)?;
            let exe = std::env::current_exe().context("could not locate the orqella binary")?;
            let path =
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
            std::fs::write(
                &unit,
                format!(
                    "[Unit]\nDescription=Orqella local dashboard\nAfter=graphical-session.target\n\n[Service]\nType=simple\nEnvironment=PATH={path}\nExecStart={}\n# Do not SIGTERM every module sidecar and QEMU guest when the daemon\n# restarts. The daemon stops its own sidecars, and guests keep running.\nKillMode=process\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
                    exe.display()
                ),
            )?;
            run_status(std::process::Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run_status(std::process::Command::new("systemctl").args([
                "--user",
                "enable",
                "--now",
                "orqella.service",
            ]))?;
            println!("installed and started orqella.service");
        }
        ServiceCommand::Uninstall => {
            let _ = run_status(std::process::Command::new("systemctl").args([
                "--user",
                "disable",
                "--now",
                "orqella.service",
            ]));
            let _ = std::fs::remove_file(&unit);
            let _ = run_status(
                std::process::Command::new("systemctl").args(["--user", "daemon-reload"]),
            );
            println!("removed orqella.service");
        }
        ServiceCommand::Status => {
            run_status(std::process::Command::new("systemctl").args([
                "--user",
                "status",
                "orqella.service",
                "--no-pager",
            ]))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_service(command: ServiceCommand, config: &Config) -> Result<()> {
    let agents = dirs::home_dir()
        .context("home directory unavailable")?
        .join("Library/LaunchAgents");
    let logs = dirs::home_dir()
        .context("home directory unavailable")?
        .join("Library/Logs/Orqella");
    let plist = agents.join("com.orqella.daemon.plist");
    let label = "com.orqella.daemon";
    match command {
        ServiceCommand::Install => {
            std::fs::create_dir_all(&agents)?;
            std::fs::create_dir_all(&logs)?;
            let exe = std::env::current_exe().context("could not locate the orqella binary")?;
            let path =
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
            let exe = xml_escape(&exe.to_string_lossy());
            let path = xml_escape(&path);
            let stdout = xml_escape(&logs.join("orqella.out.log").to_string_lossy());
            let stderr = xml_escape(&logs.join("orqella.err.log").to_string_lossy());
            std::fs::write(
                &plist,
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>serve</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{stdout}</string>
  <key>StandardErrorPath</key><string>{stderr}</string>
</dict></plist>
"#
                ),
            )?;
            let plist_arg = plist.to_string_lossy().to_string();
            let uid = unsafe { libc::getuid() };
            let domain = format!("gui/{uid}");
            let service = format!("{domain}/{label}");
            // A stale registration from an older install is harmless; remove it
            // before bootstrapping the new plist. `bootout` is the modern
            // replacement for the deprecated `unload`.
            let _ = launchctl_output(&["bootout", &service]);
            let output = launchctl_output(&["bootstrap", &domain, &plist_arg])?;
            if !output.status.success() {
                bail!(
                    "launchctl bootstrap failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let _ = launchctl_output(&["kickstart", "-k", &service]);
            println!("installed and started LaunchAgent {label}");
            let missing = orqella::hosts::missing(config);
            if !missing.is_empty() {
                use std::io::IsTerminal;
                let interactive = std::io::stdin().is_terminal()
                    && std::env::var_os("ORQELLA_SKIP_HOSTS").is_none()
                    && std::env::var_os("ORQELLA_UPDATE_PARENT_PID").is_none();
                if interactive {
                    println!("adding Safari host entries for {}", missing.join(", "));
                    if let Err(error) = orqella::hosts::install(config) {
                        println!("warning: could not update /etc/hosts: {error:#}");
                    }
                } else {
                    println!(
                        "warning: Safari cannot resolve {} until `orqella hosts install` is run",
                        missing.join(", ")
                    );
                }
            }
        }
        ServiceCommand::Uninstall => {
            let uid = unsafe { libc::getuid() };
            let service = format!("gui/{uid}/{label}");
            let output = launchctl_output(&["bootout", &service])?;
            if !output.status.success() {
                let message = String::from_utf8_lossy(&output.stderr).to_lowercase();
                let already_unloaded = [
                    "could not find service",
                    "no such process",
                    "service not found",
                    "not currently loaded",
                    "boot-out failed: 3",
                ]
                .iter()
                .any(|text| message.contains(text));
                if !already_unloaded {
                    bail!(
                        "launchctl bootout failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
            }
            let _ = std::fs::remove_file(&plist);
            println!("removed LaunchAgent {label}");
        }
        ServiceCommand::Status => {
            let uid = unsafe { libc::getuid() };
            let output = launchctl_output(&["print", &format!("gui/{uid}/{label}")])?;
            if !output.status.success() {
                bail!(
                    "LaunchAgent {label} is not loaded: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn launchctl_output(args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .context("could not run launchctl")
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "windows")]
fn windows_service(command: ServiceCommand) -> Result<()> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    match command {
        ServiceCommand::Install => {
            let exe = std::env::current_exe().context("could not locate the orqella binary")?;
            let value = format!("\"{}\" serve", exe.display());
            run_status(std::process::Command::new("reg").args([
                "add",
                key,
                "/v",
                "Orqella",
                "/t",
                "REG_SZ",
                "/d",
                value.as_str(),
                "/f",
            ]))?;
            println!("installed per-user Windows startup entry");
        }
        ServiceCommand::Uninstall => {
            run_status(
                std::process::Command::new("reg").args(["delete", key, "/v", "Orqella", "/f"]),
            )?;
            println!("removed per-user Windows startup entry");
        }
        ServiceCommand::Status => {
            run_status(std::process::Command::new("reg").args(["query", key, "/v", "Orqella"]))?;
        }
    }
    Ok(())
}

fn run_status(command: &mut std::process::Command) -> Result<()> {
    let status = command.status().context("could not run command")?;
    if !status.success() {
        bail!("command exited with {status}");
    }
    Ok(())
}

async fn doctor(paths: Paths, config: Config) -> Result<()> {
    println!("Orqella {}", env!("CARGO_PKG_VERSION"));
    println!("config      {}", paths.config_file.display());
    println!("data        {}", paths.data_dir.display());
    println!("modules     {}", paths.modules_dir.display());
    println!(
        "dashboard   {}",
        if config.server.dashboard {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("origin      {}", config.origin());
    let health_url = format!("{}/healthz", config.origin());
    match reqwest::Client::new()
        .get(&health_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            let value: serde_json::Value = response.json().await.unwrap_or_default();
            let version = value
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let commit = value
                .get("commit")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let suffix = if commit != "unknown" {
                format!(" · {commit}")
            } else {
                String::new()
            };
            println!("daemon      running {version}{suffix}");
            if version != env!("CARGO_PKG_VERSION") {
                println!(
                    "warning: running daemon is {version}, this binary is {}; restart the service",
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
        Ok(response) => println!("daemon      responded with HTTP {}", response.status()),
        Err(_) => println!("daemon      not responding at {health_url}"),
    }
    #[cfg(target_os = "macos")]
    {
        let missing = orqella::hosts::missing(&config);
        if !missing.is_empty() {
            println!(
                "safari      missing hosts entries: {} (run `orqella hosts install`)",
                missing.join(", ")
            );
        } else {
            println!("safari      hosts entries present");
        }
    }
    println!(
        "git         {}",
        if util::git_available() {
            "available"
        } else {
            "not found"
        }
    );
    let store = modules::ModuleStore::load(paths.clone()).await?;
    println!("installed   {} modules", store.list().len());
    for module in store.list() {
        println!(
            "  {:<20} {:<10} {}",
            module.manifest.id,
            module.manifest.version,
            if module.enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
    }
    Ok(())
}

fn uninstall(paths: Paths, config: &Config, purge: bool, yes: bool) -> Result<()> {
    if purge && !yes {
        print!(
            "Delete Orqella config, data, modules, and service state under {} and {}? [y/N] ",
            paths.config_dir.display(),
            paths.data_dir.display()
        );
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("uninstall cancelled");
        }
    }
    if let Err(error) = service_command(ServiceCommand::Uninstall, config) {
        eprintln!("warning: could not remove the background service: {error:#}");
    }
    if purge {
        remove_local_state(&paths)?;
    } else {
        println!("kept config and data at {}", paths.config_dir.display());
        println!("kept module data at {}", paths.data_dir.display());
        println!("run `orqella uninstall --purge` to delete them");
    }
    Ok(())
}

fn remove_local_state(paths: &Paths) -> Result<()> {
    for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir] {
        // Refuse to remove anything that is not clearly an Orqella-owned
        // directory. This protects against a malformed Paths value.
        if dir.file_name().and_then(|name| name.to_str()) != Some("orqella") {
            bail!("refusing to remove unexpected directory {}", dir.display());
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => println!("removed {}", dir.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("could not remove {}", dir.display()));
            }
        }
    }
    Ok(())
}

fn default_archive_path(kind: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("orqella-{kind}-{stamp}.zip"))
}

fn open_url(url: &str) {
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![url]);
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", url]);
    let _ = std::process::Command::new(command.0)
        .args(command.1)
        .status();
}

fn scaffold_module(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    std::fs::create_dir_all(path.join("ui"))?;
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("my-module")
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    std::fs::write(
        path.join("module.toml"),
        format!(
            "schema = 1\nid = \"{id}\"\nname = \"My module\"\nversion = \"0.1.0\"\napi_version = \"1\"\ndescription = \"A small Orqella page.\"\nentry = \"ui/index.html\"\nnode_scope = \"node\"\npermissions = [\"node.metrics\"]\n\n[ui]\nsurface = \"page\"\n\n[backend]\nkind = \"none\"\n"
        ),
    )?;
    std::fs::write(
        path.join("ui/index.html"),
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>My module</title><link rel="stylesheet" href="/assets/theme.css"><link rel="stylesheet" href="/assets/module.css"><style>body{margin:0;background:var(--bg);color:var(--ink);font:15px var(--font-sans)}main{padding:28px 32px}</style></head><body><main class="stack"><header class="page-head"><div><p class="eyebrow">My module</p><h1>My module</h1></div></header><article class="card"><p id="out" class="muted">Connecting…</p></article></main><script src="app.js"></script></body></html>
"#,
    )?;
    std::fs::write(
        path.join("ui/app.js"),
        r#"window.orqella.ready(async (ctx) => {
  const m = await window.orqella.call("system.metrics");
  document.getElementById("out").textContent = `${ctx.node.name}: CPU ${Math.round(m.cpu_percent)}% · memory ${Math.round((m.memory_used / m.memory_total) * 100)}%`;
});"#,
    )?;
    std::fs::write(
        path.join("AGENTS.md"),
        include_str!("../docs/MODULE_AGENT.md"),
    )?;
    std::fs::write(
        path.join("README.md"),
        "# My module\n\nAn Orqella module repository. Run `orqella module check .`, then `orqella module dev .`. Publish it with `orqella module publish <id>` after connecting GitHub.\n",
    )?;
    std::fs::write(
        path.join(".gitignore"),
        "/data/\n.venv/\nnode_modules/\ntarget/\n__pycache__/\n/sidecar.log\n/.config/\n/.cache/\n/.local/\n",
    )?;
    if util::init_git_repo(path) {
        println!("created {} (git initialized)", path.display());
    } else {
        println!(
            "created {} (git unavailable; changes cannot be rolled back with git)",
            path.display()
        );
    }
    Ok(())
}

fn check_module(path: &Path) -> Result<()> {
    let manifest = modules::Manifest::load(path)?;
    println!(
        "valid: {} {} ({})",
        manifest.name, manifest.version, manifest.id
    );
    println!(
        "permissions: {}",
        if manifest.permissions.is_empty() {
            "none".into()
        } else {
            manifest.permissions.join(", ")
        }
    );
    if !manifest.dependencies.is_empty() {
        println!(
            "dependencies: {}",
            manifest
                .dependencies
                .iter()
                .map(|dependency| {
                    format!(
                        "{}{}",
                        dependency.id,
                        if dependency.optional {
                            " (optional)"
                        } else {
                            " (required)"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if util::git_available() {
        if !util::is_git_repo(path) {
            println!(
                "warning: this module is not a git repository; changes cannot be rolled back with git."
            );
        } else {
            println!("git: module is under version control");
        }
    } else {
        println!("warning: git is not installed; module changes cannot be rolled back with git.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_core_commands_and_defaults() {
        let cli = Cli::try_parse_from(["orqella"]).unwrap();
        assert!(cli.command.is_none());

        let cli = Cli::try_parse_from(["orqella", "serve", "--port", "9478"]).unwrap();
        match cli.command {
            Some(Command::Serve {
                port,
                no_dashboard,
                web,
            }) => {
                assert_eq!(port, Some(9478));
                assert!(!no_dashboard);
                assert!(!web);
            }
            _ => panic!("unexpected command"),
        }

        let cli = Cli::try_parse_from(["orqella", "module", "new", "./demo", "--force"]).unwrap();
        match cli.command {
            Some(Command::Module {
                command: ModuleCommand::New { path, force },
            }) => {
                assert_eq!(path, PathBuf::from("./demo"));
                assert!(force);
            }
            _ => panic!("unexpected command"),
        }
        assert!(Cli::try_parse_from(["orqella", "module", "unknown"]).is_err());
    }

    #[test]
    fn scaffold_creates_a_valid_module_and_refuses_to_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("My Demo");
        scaffold_module(&path, false).unwrap();
        for file in [
            "module.toml",
            "ui/index.html",
            "ui/app.js",
            "AGENTS.md",
            "README.md",
            ".gitignore",
        ] {
            assert!(path.join(file).is_file(), "{file} is missing");
        }
        let manifest = modules::Manifest::load(&path).unwrap();
        assert_eq!(manifest.id, "my-demo");
        assert_eq!(manifest.entry.as_deref(), Some("ui/index.html"));
        check_module(&path).unwrap();

        let error = scaffold_module(&path, false).unwrap_err();
        assert!(format!("{error:#}").contains("--force"));
        scaffold_module(&path, true).unwrap();
    }

    #[test]
    fn check_module_rejects_an_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(check_module(root.path()).is_err());
    }

    #[test]
    fn default_archive_paths_are_zip_files_named_by_kind() {
        for kind in ["backup", "support"] {
            let path = default_archive_path(kind);
            assert_eq!(
                path.extension().and_then(|value| value.to_str()),
                Some("zip")
            );
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(name.starts_with(&format!("orqella-{kind}-")), "{name}");
        }
    }

    fn test_paths(root: &Path) -> Paths {
        Paths {
            config_dir: root.join("config/orqella"),
            data_dir: root.join("data/orqella"),
            runtime_dir: root.join("run/orqella"),
            config_file: root.join("config/orqella/config.toml"),
            state_file: root.join("data/orqella/state.json"),
            modules_dir: root.join("data/orqella/modules"),
            workshop_dir: root.join("data/orqella/workshop"),
            logs_dir: root.join("data/orqella/logs"),
        }
    }

    #[test]
    fn purge_removes_only_orqella_named_directories() {
        let root = tempfile::tempdir().unwrap();
        let paths = test_paths(root.path());
        for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir] {
            std::fs::create_dir_all(dir).unwrap();
        }
        remove_local_state(&paths).unwrap();
        for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir] {
            assert!(!dir.exists(), "{} should be removed", dir.display());
        }

        let mut bad = test_paths(root.path());
        bad.config_dir = root.path().join("not-orqella");
        std::fs::create_dir_all(&bad.config_dir).unwrap();
        let error = remove_local_state(&bad).unwrap_err();
        assert!(format!("{error:#}").contains("refusing"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xml_escaping_covers_service_values() {
        assert_eq!(
            xml_escape("a & b <tag> \"quoted\""),
            "a &amp; b &lt;tag&gt; &quot;quoted&quot;"
        );
    }
}
