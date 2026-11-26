//! First-run provisioning for the default agent workspace.
//!
//! `orqella setup` keeps the one-command installer small: the installer only
//! places the binary, then this module installs the first-party Agent Harness
//! module, installs the bundled agent skill, grants the access that module
//! declares, detects the coding agents already on the machine, and points
//! Orqella at a usable default. It deliberately does not require Git: the
//! module index resolves a pinned tree and GitHub commit archives are accepted
//! when the index source is a git URL.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Config, OFFICIAL_MODULE_INDEX_URL, Paths, ProxyConfig};
use crate::modules::index::{self, MODULE_API_VERSION};
use crate::modules::manifest::host_platform;
use crate::modules::store::{ModuleStore, ModuleView};

pub const AGENT_HARNESS_ID: &str = "agent-harness";

/// The access a first-party Agent Harness install needs. Anything a future
/// module version adds stays pending until the user reviews it.
const AGENT_HARNESS_GRANTS: [&str; 4] = ["backend", "storage", "app.open", "fs.pick"];

const OPENCODE_BASE_URL: &str = "http://127.0.0.1:49374";
const OPENCODE_PROXY_HOST: &str = "opencode.orqella.localhost";
const DOWNLOAD_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub agent_harness: bool,
    pub grant_permissions: bool,
    pub install_opencode: bool,
    pub status_only: bool,
    /// Never stop a running daemon. Installers that were asked not to manage
    /// the service pass this so setup leaves it untouched.
    pub no_restart: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            agent_harness: true,
            grant_permissions: true,
            install_opencode: false,
            status_only: false,
            no_restart: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SetupReport {
    pub agent_harness: AgentHarnessReport,
    pub harnesses: Vec<HarnessReport>,
    pub default_harness: String,
    pub agent: AgentReport,
    pub opencode: OpencodeReport,
    pub python: ToolReport,
    pub node: ToolReport,
    /// True when no usable coding agent was found and OpenCode is not
    /// installed. Installers use this to offer OpenCode.
    pub needs_opencode: bool,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentHarnessReport {
    pub installed: bool,
    pub version: String,
    pub enabled: bool,
    pub granted_permissions: Vec<String>,
    pub pending_permissions: Vec<String>,
    pub sidecar: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessReport {
    pub id: String,
    pub label: String,
    pub installed: bool,
    pub usable: bool,
    pub command: String,
    /// The resolved integration executable (an ACP adapter when present).
    pub path: String,
    /// The base CLI used by the daemon's headless agent. This is deliberately
    /// separate from `path`: `claude-agent-acp` is the interactive adapter,
    /// while `claude` is the CLI that accepts a one-shot prompt.
    pub headless_path: String,
    /// `path`, `npx`, or `adapter-missing`.
    pub source: String,
    pub login_hint: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentReport {
    pub preset: String,
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OpencodeReport {
    pub installed: bool,
    pub path: String,
    pub service_running: bool,
    pub service_url: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OpencodeSetup {
    pub opencode: OpencodeReport,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolReport {
    pub found: bool,
    pub path: String,
    pub version: String,
}

#[derive(Clone, Copy)]
struct HarnessSpec {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    adapter: Option<&'static str>,
    login_hint: &'static str,
}

const HARNESSES: [HarnessSpec; 4] = [
    HarnessSpec {
        id: "opencode",
        label: "OpenCode",
        command: "opencode",
        adapter: None,
        login_hint: "Run `opencode` in a terminal and finish onboarding.",
    },
    HarnessSpec {
        id: "claude",
        label: "Claude Code",
        command: "claude",
        adapter: Some("claude-agent-acp"),
        login_hint: "Run `claude` in a terminal and sign in.",
    },
    HarnessSpec {
        id: "codex",
        label: "Codex CLI",
        command: "codex",
        adapter: Some("codex-acp"),
        login_hint: "Run `codex login` in a terminal.",
    },
    HarnessSpec {
        id: "gemini",
        label: "Gemini CLI",
        command: "gemini",
        adapter: None,
        login_hint: "Run `gemini` in a terminal and sign in.",
    },
];

/// Detect installed coding agents without changing anything.
pub fn detect_harnesses() -> Vec<HarnessReport> {
    let npx = find_binary("npx");
    HARNESSES
        .iter()
        .map(|spec| {
            let base = find_binary(spec.command);
            let adapter = spec.adapter.and_then(find_binary);
            let (installed, usable, source, path) = match (base.as_ref(), adapter.as_ref()) {
                (_, Some(adapter)) => (true, true, "path", adapter.clone()),
                (Some(base), None) if spec.adapter.is_some() && npx.is_some() => {
                    (true, true, "npx", base.clone())
                }
                (Some(base), None) if spec.adapter.is_none() => (true, true, "path", base.clone()),
                (Some(base), None) => (true, false, "adapter-missing", base.clone()),
                (None, None) => (false, false, "", PathBuf::new()),
            };
            HarnessReport {
                id: spec.id.into(),
                label: spec.label.into(),
                installed,
                usable,
                command: spec.command.into(),
                path: path.display().to_string(),
                headless_path: base
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                source: source.into(),
                login_hint: spec.login_hint.into(),
            }
        })
        .collect()
}

fn usable_harnesses(harnesses: &[HarnessReport]) -> impl Iterator<Item = &HarnessReport> {
    harnesses.iter().filter(|harness| harness.usable)
}

fn preferred_harness<'a>(
    config: &Config,
    harnesses: &'a [HarnessReport],
) -> Option<&'a HarnessReport> {
    let current = config.agent.effective_preset();
    if current != "custom"
        && let Some(harness) = harnesses
            .iter()
            .find(|harness| harness.id == current && harness.usable)
    {
        return Some(harness);
    }
    usable_harnesses(harnesses).next()
}

/// True when the machine has no usable coding agent and OpenCode is absent.
pub fn needs_opencode(config: &Config) -> bool {
    if config.agent.effective_preset() == "custom" && find_binary(&config.agent.command).is_some() {
        return false;
    }
    let harnesses = detect_harnesses();
    preferred_harness(config, &harnesses).is_none()
        && !harnesses.iter().any(|h| h.id == "opencode" && h.installed)
}

/// Read the current setup state without installing modules, changing Orqella
/// config, or starting services.
pub async fn status(paths: &Paths, config: &Config) -> Result<SetupReport> {
    let mut report = SetupReport {
        harnesses: detect_harnesses(),
        python: detect_python().await,
        node: detect_node(),
        ..SetupReport::default()
    };
    report.agent = AgentReport {
        preset: config.agent.preset.clone(),
        command: config.agent.command.clone(),
    };
    report.opencode = opencode_status(&report.harnesses, false).await;
    report.default_harness = preferred_harness(config, &report.harnesses)
        .map(|h| h.id.clone())
        .unwrap_or_default();
    if let Ok(store) = ModuleStore::load(paths.clone()).await
        && let Some(view) = store
            .list()
            .into_iter()
            .find(|m| m.manifest.id == AGENT_HARNESS_ID)
    {
        report.agent_harness = agent_harness_report(&view, "installed");
    }
    report.needs_opencode = needs_opencode(config);
    Ok(report)
}

/// Provision the default agent workspace. Failures that only affect an
/// optional integration are reported as warnings so the daemon install still
/// succeeds.
pub async fn run(
    paths: &Paths,
    config: &mut Config,
    options: &SetupOptions,
) -> Result<SetupReport> {
    if options.status_only {
        return status(paths, config).await;
    }

    // The setup path updates the module store and config directly. A running
    // daemon has both in memory and could overwrite the changes on shutdown,
    // so stop the managed service and bring it back afterward. A manual
    // `orqella serve` is the user's process; leave it alone and explain how to
    // finish.
    let state = daemon_state(paths, config).await;
    match state {
        DaemonState::NotRunning => {}
        DaemonState::Manual => bail!(
            "the Orqella daemon is running; stop your manual `orqella serve`, run `orqella setup`, then start Orqella again"
        ),
        DaemonState::Managed if options.no_restart => bail!(
            "the Orqella daemon is running; stop it, run `orqella setup`, then start Orqella again"
        ),
        DaemonState::Managed => {}
    }

    // The OpenCode download is the slow part and does not touch Orqella state.
    // Do it before stopping a managed daemon to keep the dashboard available.
    let early_opencode = if options.install_opencode {
        Some(install_opencode().await)
    } else {
        None
    };

    let restart = matches!(state, DaemonState::Managed);
    if restart {
        stop_managed_service(paths).await?;
    }

    let mut result = provision(paths, config, options, early_opencode).await;
    if restart {
        match restart_managed_service().await {
            Ok(()) => {
                if let Ok(report) = &mut result {
                    report
                        .actions
                        .push("Restarted Orqella to apply the setup".into());
                }
            }
            Err(error) => {
                if let Ok(report) = &mut result {
                    report.warnings.push(format!(
                        "setup finished, but the Orqella service did not restart: {error:#}"
                    ));
                } else {
                    return Err(error);
                }
            }
        }
    }
    result
}

async fn provision(
    paths: &Paths,
    config: &mut Config,
    options: &SetupOptions,
    early_opencode: Option<Result<(PathBuf, bool)>>,
) -> Result<SetupReport> {
    let mut report = SetupReport {
        python: detect_python().await,
        node: detect_node(),
        ..SetupReport::default()
    };
    if !report.python.found {
        report.warnings.push(
            "Python 3.9+ was not found. The Agent Harness sidecar will not start until Python is installed."
                .into(),
        );
    } else if python_too_old(&report.python.version) {
        report.warnings.push(format!(
            "Python {} is too old for the Agent Harness sidecar; install Python 3.9 or newer.",
            report.python.version
        ));
    }

    let mut opencode_installed_this_run = false;
    if let Some(result) = early_opencode {
        match result {
            Ok((path, installed)) => {
                opencode_installed_this_run = installed;
                if installed {
                    report
                        .actions
                        .push(format!("Installed OpenCode at {}", path.display()));
                } else {
                    report.actions.push(format!(
                        "OpenCode is already installed at {}",
                        path.display()
                    ));
                }
            }
            Err(error) => report
                .warnings
                .push(format!("Could not install OpenCode: {error:#}")),
        }
    }

    report.harnesses = detect_harnesses();
    for harness in &report.harnesses {
        if harness.source == "adapter-missing" {
            report.warnings.push(format!(
                "{} was found, but its ACP adapter and Node/npx are missing. Install Node.js or the adapter to use it in the workspace.",
                harness.label
            ));
        }
        if harness.installed && harness.headless_path.is_empty() {
            report.warnings.push(format!(
                "{} has an ACP adapter but its base CLI was not found; the headless Build agent will not use it.",
                harness.label
            ));
        }
    }

    // Install the bundled skill into the paths coding agents discover. This
    // keeps module work grounded in the Orqella contracts even when the user
    // does not have this repository checked out.
    match crate::skill::install_default() {
        Ok(results) => {
            for (path, changed) in results {
                if changed {
                    report.actions.push(format!(
                        "Installed the Orqella agent skill at {}",
                        path.join("SKILL.md").display()
                    ));
                }
            }
        }
        Err(error) => report.warnings.push(format!(
            "Could not install the Orqella agent skill: {error:#}"
        )),
    }

    if options.agent_harness {
        match ensure_agent_harness(paths, config, options.grant_permissions).await {
            Ok(outcome) => {
                report.agent_harness =
                    agent_harness_report(&outcome.view, outcome.sidecar.as_str());
                report.warnings.extend(outcome.warnings);
                if outcome.installed {
                    report.actions.push(format!(
                        "Installed Agent Harness {}",
                        outcome.view.manifest.version
                    ));
                }
                if !outcome.granted.is_empty() {
                    report.actions.push(format!(
                        "Granted Agent Harness: {}",
                        outcome.granted.join(", ")
                    ));
                }
                let selected = preferred_harness(config, &report.harnesses).cloned();
                let harnesses = report.harnesses.clone();
                if let Some(selected) = selected {
                    apply_module_config(
                        paths,
                        &outcome.store,
                        &outcome.view,
                        &selected,
                        &harnesses,
                        outcome.installed,
                        &mut report,
                    );
                }
            }
            Err(error) => report
                .warnings
                .push(format!("Could not install Agent Harness: {error:#}")),
        }
    }

    report.opencode = opencode_status(&report.harnesses, true).await;
    if report.opencode.installed
        && !report.opencode.service_running
        && !report.opencode.service_url.is_empty()
    {
        report.warnings.push(format!(
            "OpenCode is installed but its background service is not reachable at {}.",
            report.opencode.service_url
        ));
    }
    if opencode_installed_this_run && report.opencode.installed {
        match configure_opencode_free_model(&report.opencode.path).await {
            Ok(Some((model, path))) => {
                report.opencode.model = model.clone();
                report.actions.push(format!(
                    "Set OpenCode's default model to {model} ({})",
                    path.display()
                ));
            }
            Ok(None) => {
                report
                    .actions
                    .push("OpenCode's default model was left unchanged.".into());
            }
            Err(error) => report.warnings.push(format!(
                "OpenCode is installed, but its default free model could not be selected: {error:#}"
            )),
        }
    }

    let selected = preferred_harness(config, &report.harnesses).cloned();
    report.default_harness = selected
        .as_ref()
        .map(|harness| harness.id.clone())
        .unwrap_or_default();
    if let Some(selected) = selected.as_ref() {
        apply_daemon_config(
            config,
            selected,
            report.opencode.service_port(),
            &mut report,
        );
    }
    report.agent = AgentReport {
        preset: config.agent.preset.clone(),
        command: config.agent.command.clone(),
    };
    report.needs_opencode = needs_opencode(config);

    if selected.is_none() {
        report.warnings.push(
            "No coding agent was found. Install OpenCode with `orqella setup --install-opencode` or add one in Settings → Agents."
                .into(),
        );
    }
    // Persist before the caller restarts a managed daemon so the new process
    // starts from the setup config, not the pre-setup file.
    config
        .save(paths)
        .context("could not save the setup config")?;
    Ok(report)
}

struct HarnessOutcome {
    store: Arc<ModuleStore>,
    view: ModuleView,
    installed: bool,
    granted: Vec<String>,
    sidecar: String,
    warnings: Vec<String>,
}

async fn ensure_agent_harness(
    paths: &Paths,
    config: &Config,
    grant_permissions: bool,
) -> Result<HarnessOutcome> {
    let store = Arc::new(ModuleStore::load(paths.clone()).await?);
    if let Some(view) = store
        .list()
        .into_iter()
        .find(|m| m.manifest.id == AGENT_HARNESS_ID)
    {
        return Ok(HarnessOutcome {
            store,
            view,
            installed: false,
            granted: Vec::new(),
            sidecar: "installed".into(),
            warnings: Vec::new(),
        });
    }

    let client = external_client()?;
    let index = index::fetch(&client, &config.modules.index_url).await?;
    let official = index
        .module(AGENT_HARNESS_ID)
        .map(|module| module.official)
        .unwrap_or(false);
    let platform = host_platform();
    let target = index.resolve(AGENT_HARNESS_ID, None, &platform, MODULE_API_VERSION)?;
    let installed = index::install(&client, Arc::clone(&store), target).await?;

    let mut granted = Vec::new();
    let mut warnings = Vec::new();
    let official_index = config.modules.index_url == OFFICIAL_MODULE_INDEX_URL;
    if grant_permissions && official && official_index {
        let declared: Vec<String> = installed
            .manifest
            .permissions
            .iter()
            .filter(|permission| AGENT_HARNESS_GRANTS.contains(&permission.as_str()))
            .cloned()
            .collect();
        match store.set_permissions(AGENT_HARNESS_ID, declared) {
            Ok(view) => {
                granted = view.granted_permissions.clone();
                return Ok(HarnessOutcome {
                    store,
                    view,
                    installed: true,
                    granted,
                    sidecar: "installed".into(),
                    warnings,
                });
            }
            Err(error) => {
                warnings.push(format!(
                    "Agent Harness was installed, but its first-party access could not be granted: {error:#}. Review it in Settings → Modules."
                ));
            }
        }
    }
    Ok(HarnessOutcome {
        store,
        view: installed,
        installed: true,
        granted,
        sidecar: "installed".into(),
        warnings,
    })
}

fn apply_module_config(
    paths: &Paths,
    store: &ModuleStore,
    view: &ModuleView,
    selected: &HarnessReport,
    harnesses: &[HarnessReport],
    installed_this_run: bool,
    report: &mut SetupReport,
) {
    let mut values = view.config.clone();
    let current_default = values
        .get("default_harness")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current_is_usable = harnesses
        .iter()
        .any(|harness| harness.id == current_default && harness.usable);
    let set_default = installed_this_run || current_default.is_empty() || !current_is_usable;
    if set_default {
        values.insert("default_harness".into(), json!(selected.id));
    }
    let needs_fallback = harnesses
        .iter()
        .any(|harness| harness.usable && harness.source == "npx");
    if needs_fallback {
        values.insert("allow_adapter_install".into(), json!(true));
    }
    if values != view.config {
        match store.set_config(
            AGENT_HARNESS_ID,
            values.into_iter().collect::<BTreeMap<_, _>>(),
        ) {
            Ok(updated) => {
                report.agent_harness = agent_harness_report(&updated, "installed");
            }
            Err(error) => report
                .warnings
                .push(format!("Could not save Agent Harness settings: {error:#}")),
        }
    }

    // An older install already has harnesses.json. Its stored default is the
    // registry's source of truth, so update it only when that harness is no
    // longer usable. A valid user choice is never replaced.
    let registry_path = paths
        .module_dir(AGENT_HARNESS_ID)
        .join("data/harnesses.json");
    let Ok(raw) = std::fs::read_to_string(&registry_path) else {
        return;
    };
    let Ok(mut registry) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let stored_default = registry
        .get("default")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stored_usable = harnesses
        .iter()
        .any(|harness| harness.id == stored_default && harness.usable);
    // A registry can contain user-defined exec harnesses. Their ids are not in
    // HARNESSES, so the built-in detection above cannot prove they are
    // unusable. Preserve an enabled custom entry when its command still
    // resolves. The built-in `custom` entry is handled the same way.
    let builtin = HARNESSES.iter().any(|harness| harness.id == stored_default);
    let stored_custom_usable = registry
        .get("harnesses")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(Value::as_str) == Some(stored_default))
        })
        .filter(|row| row.get("enabled").and_then(Value::as_bool) != Some(false))
        .and_then(|row| row.get("command").and_then(Value::as_str))
        .filter(|command| !command.is_empty())
        .is_some_and(|command| find_binary(command).is_some());
    if stored_usable
        || stored_default == selected.id
        || (stored_custom_usable && (!builtin || stored_default == "custom"))
    {
        return;
    }
    if let Some(object) = registry.as_object_mut() {
        object.insert("default".into(), json!(selected.id));
        let mut bytes = serde_json::to_vec_pretty(&registry).unwrap_or_default();
        bytes.push(b'\n');
        if let Err(error) = crate::util::write_atomic(&registry_path, &bytes) {
            report.warnings.push(format!(
                "Could not update the Agent Harness default: {error:#}"
            ));
        } else {
            report
                .actions
                .push(format!("Default agent set to {}", selected.label));
        }
    }
}

fn ensure_opencode_proxy(
    config: &mut Config,
    opencode_port: Option<u16>,
    actions: &mut Vec<String>,
) {
    if config.proxy.iter().any(|mount| mount.id == "opencode") {
        return;
    }
    let port = opencode_port.unwrap_or(49374);
    config.proxy.push(ProxyConfig {
        id: "opencode".into(),
        title: "OpenCode".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        host: Some(OPENCODE_PROXY_HOST.into()),
        websocket: true,
        frame: false,
    });
    actions.push(format!(
        "Added the OpenCode app surface ({OPENCODE_PROXY_HOST})"
    ));
}

fn apply_daemon_config(
    config: &mut Config,
    selected: &HarnessReport,
    opencode_port: Option<u16>,
    report: &mut SetupReport,
) {
    if selected.id == "opencode" {
        ensure_opencode_proxy(config, opencode_port, &mut report.actions);
    }

    let current_command = config.agent.command.trim();
    let current_preset = config.agent.effective_preset();
    // The headless agent inherits the daemon's PATH. A built-in command that
    // only exists in a well-known directory such as ~/.local/bin still needs
    // the absolute path here; otherwise a fresh OpenCode install is invisible
    // to Build and background jobs.
    let current_usable = if current_preset == "custom" {
        find_binary(current_command).is_some()
    } else {
        find_binary_on_path(current_command).is_some()
    };
    let already_selected = current_preset == selected.id;
    if current_usable && (already_selected || current_preset == "custom") {
        return;
    }
    let Some((_command, args)) = crate::config::preset_defaults(&selected.id) else {
        return;
    };
    if selected.headless_path.is_empty() {
        report.warnings.push(format!(
            "{} is available only through its ACP adapter; the headless Build agent was left unchanged.",
            selected.label
        ));
        return;
    }
    let command = selected.headless_path.clone();
    #[allow(unused_mut)]
    let mut args = args;
    #[cfg(windows)]
    let command = if matches!(
        Path::new(&command)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("cmd" | "bat")
    ) {
        // CreateProcess cannot execute a batch shim directly.
        args.insert(0, "/C".into());
        args.insert(1, command.clone());
        "cmd.exe".to_string()
    } else {
        command
    };
    config.agent.preset = selected.id.clone();
    config.agent.command = command.clone();
    config.agent.args = args;
    report.actions.push(format!(
        "Set the headless agent to {} ({command})",
        selected.label
    ));
}

fn agent_harness_report(view: &ModuleView, sidecar: &str) -> AgentHarnessReport {
    AgentHarnessReport {
        installed: true,
        version: view.manifest.version.clone(),
        enabled: view.enabled,
        granted_permissions: view.granted_permissions.clone(),
        pending_permissions: view.pending_permissions.clone(),
        sidecar: sidecar.into(),
    }
}

enum DaemonState {
    NotRunning,
    Managed,
    Manual,
}

async fn daemon_state(paths: &Paths, config: &Config) -> DaemonState {
    // A portable ORQELLA_HOME is not the machine-wide managed service. Do not
    // stop the user's normal installation while provisioning a separate one.
    let portable = std::env::var_os("ORQELLA_HOME").is_some();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = portable;
    #[cfg(target_os = "linux")]
    if !portable
        && command_ok(
            "systemctl",
            &["--user", "is-active", "--quiet", "orqella.service"],
        )
        .await
    {
        return DaemonState::Managed;
    }
    #[cfg(target_os = "macos")]
    if !portable && command_ok("launchctl", &["print", &launch_agent_label()]).await {
        return DaemonState::Managed;
    }
    if daemon_socket_alive(paths, config).await {
        DaemonState::Manual
    } else {
        DaemonState::NotRunning
    }
}

#[cfg(unix)]
async fn daemon_socket_alive(paths: &Paths, _config: &Config) -> bool {
    // The trusted socket is scoped to this config/data layout, so a different
    // portable ORQELLA_HOME on the same loopback port is not mistaken for the
    // daemon this setup would mutate.
    tokio::net::UnixStream::connect(paths.runtime_dir.join("orqella.sock"))
        .await
        .is_ok()
}

#[cfg(not(unix))]
async fn daemon_socket_alive(_paths: &Paths, config: &Config) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(800))
        .build()
    else {
        return false;
    };
    let url = format!("{}healthz", config.loopback_origin());
    client
        .get(url)
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn launch_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn launch_agent_label() -> String {
    format!("{}/com.orqella.daemon", launch_domain())
}

#[cfg(target_os = "linux")]
async fn stop_managed_service(paths: &Paths) -> Result<()> {
    if !command_ok("systemctl", &["--user", "stop", "orqella.service"]).await {
        bail!(
            "could not stop orqella.service; run `systemctl --user stop orqella.service` and retry"
        );
    }
    wait_for_socket_down(paths).await
}

#[cfg(target_os = "macos")]
async fn stop_managed_service(paths: &Paths) -> Result<()> {
    let _ = command_ok("launchctl", &["bootout", &launch_agent_label()]).await;
    wait_for_socket_down(paths).await
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn stop_managed_service(_paths: &Paths) -> Result<()> {
    bail!("managed service control is not implemented on this platform")
}

async fn wait_for_socket_down(paths: &Paths) -> Result<()> {
    for _ in 0..100 {
        if !daemon_socket_alive(paths, &Config::default()).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("the Orqella service is still running; stop it and run `orqella setup` again")
}

#[cfg(target_os = "linux")]
async fn restart_managed_service() -> Result<()> {
    if !command_ok("systemctl", &["--user", "start", "orqella.service"]).await {
        bail!("run `systemctl --user start orqella.service` to start Orqella");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn restart_managed_service() -> Result<()> {
    let label = launch_agent_label();
    if let Some(plist) = host_home()
        .map(|home| home.join("Library/LaunchAgents/com.orqella.daemon.plist"))
        .filter(|path| path.is_file())
    {
        let _ = command_ok(
            "launchctl",
            &["bootstrap", &launch_domain(), &plist.display().to_string()],
        )
        .await;
    }
    if !command_ok("launchctl", &["kickstart", "-k", &label]).await {
        bail!("run `launchctl kickstart -k {label}` to start Orqella");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn restart_managed_service() -> Result<()> {
    bail!("managed service control is not implemented on this platform")
}

async fn command_ok(program: &str, args: &[&str]) -> bool {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(15), command.status())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|status| status.success())
}

async fn download(client: &reqwest::Client, url: &str, max: u64) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?;
    if !response.status().is_success() {
        bail!("could not download {url}: HTTP {}", response.status());
    }
    if let Some(length) = response.content_length()
        && length > max
    {
        bail!("download from {url} is larger than the allowed {max} bytes");
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("could not read {url}"))?;
    if bytes.len() as u64 > max {
        bail!("download from {url} is larger than the allowed {max} bytes");
    }
    Ok(bytes.to_vec())
}

fn external_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .no_proxy()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .context("could not create the setup HTTP client")
}

// -- OpenCode -------------------------------------------------------------

async fn opencode_status(harnesses: &[HarnessReport], start_service: bool) -> OpencodeReport {
    let Some(harness) = harnesses.iter().find(|h| h.id == "opencode") else {
        return OpencodeReport::default();
    };
    let mut report = OpencodeReport {
        installed: harness.installed,
        path: harness.path.clone(),
        service_url: OPENCODE_BASE_URL.into(),
        ..OpencodeReport::default()
    };
    if !report.installed {
        return report;
    }
    let binary = PathBuf::from(&report.path);
    if start_service {
        let _ = run_command(&binary, &["service", "start"], SERVICE_START_TIMEOUT).await;
    }
    let port = opencode_service_port();
    if let Some(port) = port {
        report.service_url = format!("http://127.0.0.1:{port}");
    }
    report.service_running = match port {
        Some(port) => tokio::time::timeout(
            Duration::from_millis(700),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false),
        None => false,
    };
    report
}

impl OpencodeReport {
    fn service_port(&self) -> Option<u16> {
        self.service_url
            .rsplit(':')
            .next()
            .and_then(|port| port.trim_end_matches('/').parse().ok())
    }
}

async fn configure_opencode_free_model(binary: &str) -> Result<Option<(String, PathBuf)>> {
    let models = match run_command(
        Path::new(binary),
        &["api", "get", "/api/model"],
        Duration::from_secs(15),
    )
    .await
    {
        Ok(result) if result.status.success() => parse_opencode_models(&result.stdout),
        _ => Vec::new(),
    };
    let mut model = select_free_model(&models);
    if model.is_none()
        && let Ok(result) =
            run_command(Path::new(binary), &["models"], Duration::from_secs(15)).await
    {
        model = free_model_from_listing(&result.stdout);
    }
    let Some(model) = model else {
        return Ok(None);
    };
    let Some(path) = write_opencode_default_model(&model)? else {
        return Ok(None);
    };
    // The managed service loads the model at startup. Restart it so the next
    // session uses the free default without a manual restart.
    let _ = run_command(
        Path::new(binary),
        &["service", "restart"],
        SERVICE_START_TIMEOUT,
    )
    .await;
    Ok(Some((model, path)))
}

fn parse_opencode_models(output: &str) -> Vec<Value> {
    let value: Value = match serde_json::from_str(output.trim()) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn select_free_model(models: &[Value]) -> Option<String> {
    let mut free = models
        .iter()
        .filter(|model| {
            model.get("providerID").and_then(Value::as_str) == Some("opencode")
                && model.get("type").and_then(Value::as_str) == Some("chat")
                && model.get("enabled").and_then(Value::as_bool) != Some(false)
                && !matches!(
                    model.get("status").and_then(Value::as_str),
                    Some("deprecated" | "alpha")
                )
                && model_is_free(model)
        })
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?;
            let released = model
                .pointer("/time/released")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some((released, id.to_string()))
        })
        .collect::<Vec<_>>();
    free.sort_by_key(|(released, _)| std::cmp::Reverse(*released));
    free.into_iter()
        .next()
        .map(|(_, id)| format!("opencode/{id}"))
}

fn model_is_free(model: &Value) -> bool {
    let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
    let named_free = id.ends_with("-free") || id == "big-pickle";
    let Some(cost) = model.get("cost").and_then(Value::as_array) else {
        return named_free;
    };
    if cost.is_empty() {
        return named_free;
    }
    cost.iter().all(|entry| {
        let input = entry.get("input").and_then(Value::as_f64).unwrap_or(0.0);
        let output = entry.get("output").and_then(Value::as_f64).unwrap_or(0.0);
        let cache = entry.get("cache").and_then(Value::as_object);
        let cache_zero = cache
            .map(|cache| {
                cache
                    .values()
                    .all(|value| value.as_f64().unwrap_or(0.0) == 0.0)
            })
            .unwrap_or(true);
        input == 0.0 && output == 0.0 && cache_zero
    })
}

fn free_model_from_listing(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("opencode/"))
        .find(|line| line.contains("-free") || *line == "opencode/big-pickle")
        .map(str::to_string)
}

fn write_opencode_default_model(model: &str) -> Result<Option<PathBuf>> {
    let Some(home) = host_home() else {
        return Ok(None);
    };
    // OpenCode's user-level config is always ~/.config/opencode, including on
    // Windows. System-wide config lives elsewhere and is deliberately not
    // touched.
    let base = home.join(".config/opencode");
    let candidates = [base.join("opencode.json"), base.join("opencode.jsonc")];
    let existing = candidates.iter().find(|path| path.is_file());
    if let Some(path) = existing {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let mut value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => {
                // JSONC comments cannot be merged safely without a JSONC
                // parser. Leave the user's file untouched.
                return Ok(None);
            }
        };
        let Some(object) = value.as_object_mut() else {
            return Ok(None);
        };
        if object.contains_key("model") {
            return Ok(None);
        }
        object.insert("model".into(), json!(model));
        std::fs::write(path, serde_json::to_string_pretty(&value)? + "\n")
            .with_context(|| format!("could not write {}", path.display()))?;
        return Ok(Some(path.clone()));
    }

    std::fs::create_dir_all(&base)
        .with_context(|| format!("could not create {}", base.display()))?;
    let path = base.join("opencode.json");
    let value = json!({
        "$schema": "https://opencode.ai/config.json",
        "model": model,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&value)? + "\n")
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(Some(path))
}

/// Install the official prebuilt OpenCode binary. Returns the path and whether
/// this call installed it (false when a usable copy was already present).
pub async fn install_opencode() -> Result<(PathBuf, bool)> {
    if let Some(existing) = find_binary("opencode") {
        return Ok((existing, false));
    }
    let (archive, format) = opencode_download()?;
    tracing::info!(url = %archive, "downloading OpenCode");
    let client = external_client()?;
    let bytes = download(&client, &archive, DOWNLOAD_MAX_BYTES).await?;
    let binary = match format {
        "zip" => extract_zip_binary(&bytes, "opencode")?,
        _ => extract_tar_binary(&bytes)?,
    };
    let install_dir = std::env::var_os("ORQELLA_OPENCODE_INSTALL_DIR")
        .map(PathBuf::from)
        .or_else(|| host_home().map(|home| home.join(".local/bin")))
        .context("could not determine an OpenCode install directory")?;
    std::fs::create_dir_all(&install_dir)
        .with_context(|| format!("could not create {}", install_dir.display()))?;
    let name = if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    };
    let target = install_dir.join(name);
    let temporary = install_dir.join(format!(".{name}.orqella-install"));
    std::fs::write(&temporary, binary)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&temporary, &target)
        .with_context(|| format!("could not install {}", target.display()))?;
    Ok((target, true))
}

/// Install OpenCode without touching the module store, and apply the daemon
/// settings it enables. This is the post-install path used by the dashboard
/// while the daemon is running. The caller persists `config`.
pub async fn install_opencode_integration(config: &mut Config) -> Result<OpencodeSetup> {
    let (path, installed) = install_opencode().await?;
    let mut setup = OpencodeSetup::default();
    if installed {
        setup
            .actions
            .push(format!("Installed OpenCode at {}", path.display()));
    } else {
        setup.actions.push(format!(
            "OpenCode is already installed at {}",
            path.display()
        ));
    }

    let harnesses = detect_harnesses();
    setup.opencode = opencode_status(&harnesses, true).await;
    if setup.opencode.installed && !setup.opencode.service_running {
        setup.warnings.push(format!(
            "OpenCode is installed, but its background service is not reachable at {}.",
            setup.opencode.service_url
        ));
    }
    if installed {
        match configure_opencode_free_model(&path.display().to_string()).await {
            Ok(Some((model, config_path))) => {
                setup.opencode.model = model.clone();
                setup.actions.push(format!(
                    "Set OpenCode's free default model to {model} ({})",
                    config_path.display()
                ));
            }
            Ok(None) => {}
            Err(error) => setup.warnings.push(format!(
                "OpenCode is installed, but its free default model could not be selected: {error:#}"
            )),
        }
    }

    if setup.opencode.installed {
        ensure_opencode_proxy(config, setup.opencode.service_port(), &mut setup.actions);
    }
    if let Some(selected) = preferred_harness(config, &harnesses).cloned() {
        let mut report = SetupReport::default();
        apply_daemon_config(
            config,
            &selected,
            setup.opencode.service_port(),
            &mut report,
        );
        setup.actions.extend(report.actions);
        setup.warnings.extend(report.warnings);
    }
    Ok(setup)
}

fn opencode_download() -> Result<(String, &'static str)> {
    let base = "https://opencode.ai/update/api/latest/cli/opencode";
    let value = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => format!("{base}/opencode-linux-x64.tar.gz"),
        ("linux", "aarch64") => format!("{base}/opencode-linux-arm64.tar.gz"),
        ("macos", "x86_64") => format!("{base}/opencode-darwin-x64.zip"),
        ("macos", "aarch64") => format!("{base}/opencode-darwin-arm64.zip"),
        ("windows", "x86_64") => format!("{base}/opencode-windows-x64.zip"),
        ("windows", "aarch64") => format!("{base}/opencode-windows-arm64.zip"),
        _ => bail!(
            "no prebuilt OpenCode binary is published for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    };
    let format = if value.ends_with(".zip") {
        "zip"
    } else {
        "tar.gz"
    };
    Ok((value, format))
}

fn extract_tar_binary(bytes: &[u8]) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("invalid OpenCode archive")? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.file_name().and_then(|name| name.to_str()) == Some("opencode") {
            let mut output = Vec::new();
            entry.read_to_end(&mut output)?;
            return Ok(output);
        }
    }
    bail!("the OpenCode archive did not contain the opencode binary")
}

fn extract_zip_binary(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("invalid OpenCode archive")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let file_name = entry
            .name()?
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        if file_name == name || file_name == format!("{name}.exe") {
            let mut output = Vec::new();
            entry.read_to_end(&mut output)?;
            return Ok(output);
        }
    }
    bail!("the OpenCode archive did not contain the opencode binary")
}

fn opencode_service_port() -> Option<u16> {
    let home = host_home()?;
    let mut paths = vec![
        home.join(".config/opencode/service.json"),
        home.join(".local/state/opencode/service.json"),
        home.join("Library/Application Support/opencode/service.json"),
        home.join("AppData/Roaming/opencode/service.json"),
        home.join("AppData/Local/opencode/service.json"),
    ];
    paths.dedup();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(port) = value.get("port").and_then(Value::as_u64)
            && port > 0
            && port <= u16::MAX as u64
        {
            return Some(port as u16);
        }
        if let Some(url) = value.get("url").and_then(Value::as_str)
            && let Ok(parsed) = url::Url::parse(url)
            && let Some(port) = parsed.port()
        {
            return Some(port);
        }
    }
    None
}

// -- Tool detection -------------------------------------------------------

fn host_home() -> Option<PathBuf> {
    std::env::var_os("ORQELLA_HOST_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(dirs::home_dir)
}

fn find_binary_on_path(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if command.contains('/') || command.contains('\\') {
        let path = PathBuf::from(command);
        return executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in executable_names(command) {
            let candidate = directory.join(name);
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn find_binary(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if command.contains('/') || command.contains('\\') {
        let path = PathBuf::from(command);
        return executable(&path).then_some(path);
    }
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    if let Some(home) = host_home() {
        directories.extend([
            home.join(".local/bin"),
            home.join(".local/share/mise/shims"),
            home.join(".local/share/mise/command-wrappers/bin"),
            home.join(".opencode/bin"),
            home.join("AppData/Local/Programs/OpenCode"),
        ]);
    }
    directories.extend([PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]);
    let mut seen = std::collections::HashSet::new();
    for directory in directories {
        if !seen.insert(directory.clone()) {
            continue;
        }
        for name in executable_names(command) {
            let path = directory.join(name);
            if executable(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn executable_names(command: &str) -> Vec<String> {
    if cfg!(windows) {
        let mut names = vec![command.to_string()];
        for extension in ["exe", "cmd", "bat"] {
            names.push(format!("{command}.{extension}"));
        }
        names
    } else {
        vec![command.to_string()]
    }
}

fn executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn detect_node() -> ToolReport {
    let Some(path) = find_binary("node") else {
        return ToolReport::default();
    };
    ToolReport {
        found: true,
        path: path.display().to_string(),
        ..ToolReport::default()
    }
}

async fn detect_python() -> ToolReport {
    let Some(path) = find_binary("python3").or_else(|| find_binary("python")) else {
        return ToolReport::default();
    };
    let version = run_command(
        &path,
        &[
            "-c",
            "import sys; print('.'.join(map(str, sys.version_info[:3])))",
        ],
        VERSION_TIMEOUT,
    )
    .await
    .ok()
    .filter(|output| output.status.success())
    .map(|output| output.stdout.trim().to_string())
    .unwrap_or_default();
    ToolReport {
        found: true,
        path: path.display().to_string(),
        version,
    }
}

fn python_too_old(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    major < 3 || (major == 3 && minor < 9)
}

struct CommandResult {
    status: std::process::ExitStatus,
    stdout: String,
}

async fn run_command(path: &Path, args: &[&str], timeout: Duration) -> Result<CommandResult> {
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .with_context(|| format!("could not run {}", path.display()))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .with_context(|| format!("{} timed out", path.display()))?
        .with_context(|| format!("{} failed", path.display()))?;
    Ok(CommandResult {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_model_prefers_the_newest_zero_cost_model() {
        let models = vec![
            json!({
                "providerID": "opencode",
                "id": "old-free",
                "type": "chat",
                "cost": [{"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}],
                "time": {"released": 1}
            }),
            json!({
                "providerID": "opencode",
                "id": "paid",
                "type": "chat",
                "cost": [{"input": 1, "output": 2}],
                "time": {"released": 3}
            }),
            json!({
                "providerID": "opencode",
                "id": "new-free",
                "type": "chat",
                "cost": [{"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}],
                "time": {"released": 2}
            }),
        ];
        assert_eq!(
            select_free_model(&models).as_deref(),
            Some("opencode/new-free")
        );
    }

    #[test]
    fn free_model_listing_fallback_ignores_paid_models() {
        let output =
            "opencode/big-pickle\nopencode/paid\nopencode-go/gemini-4\nopencode/hy4-flash-free\n";
        assert_eq!(
            free_model_from_listing(output).as_deref(),
            Some("opencode/big-pickle")
        );
    }

    #[test]
    fn opencode_archives_extract_the_binary() {
        use std::io::Write;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "package/opencode", &b"test"[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let tar_bytes = encoder.finish().unwrap();
        assert_eq!(extract_tar_binary(&tar_bytes).unwrap(), b"test");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            "package/opencode.exe",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"test").unwrap();
        let zip_bytes = zip.finish().unwrap().into_inner();
        assert_eq!(extract_zip_binary(&zip_bytes, "opencode").unwrap(), b"test");
    }

    #[test]
    fn python_version_floor_matches_the_module() {
        assert!(!python_too_old("3.9.0"));
        assert!(!python_too_old("3.12.16"));
        assert!(python_too_old("3.8.18"));
        assert!(python_too_old(""));
    }
}
