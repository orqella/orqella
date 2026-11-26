use crate::config::{AgentConfig, Paths};
use crate::modules::manifest::Manifest;
use crate::modules::sidecar::SidecarManager;
use crate::modules::store::ModuleStore;
use crate::theme;
use crate::util;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::warn;

/// Pass the prompt as the final argument by default, or substitute `{prompt}`
/// when a custom harness expects a flag or a different argument position.
fn prompt_args(agent: &AgentConfig, prompt: &str) -> Vec<String> {
    let mut args = Vec::with_capacity(agent.args.len() + 1);
    let mut substituted = false;
    for arg in &agent.args {
        if arg.contains("{prompt}") {
            args.push(arg.replace("{prompt}", prompt));
            substituted = true;
        } else {
            args.push(arg.clone());
        }
    }
    if !substituted {
        args.push(prompt.to_string());
    }
    args
}
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildJob {
    pub id: String,
    pub prompt: String,
    pub status: String,
    pub error: Option<String>,
    pub module_id: Option<String>,
    pub workspace: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

pub struct ThemeJobRequest {
    pub base_theme: String,
    pub base_css: String,
    pub base_scheme: String,
    pub theme_id: String,
    pub fallback_name: String,
    pub fallback_description: String,
    pub prompt: String,
}

pub struct BuildManager {
    paths: Paths,
    jobs_dir: PathBuf,
    agent: RwLock<AgentConfig>,
    sidecars: Arc<SidecarManager>,
    jobs: Arc<RwLock<HashMap<String, BuildJob>>>,
    processes: Arc<RwLock<HashMap<String, u32>>>,
}

const MAX_JOBS: usize = 100;
const MAX_ACTIVE_JOBS: usize = 4;

impl BuildManager {
    pub fn new(paths: Paths, agent: AgentConfig, sidecars: Arc<SidecarManager>) -> Self {
        let jobs_dir = paths.workshop_dir.join("agent-jobs");
        let jobs = load_jobs(&jobs_dir);
        let manager = Self {
            paths,
            jobs_dir,
            agent: RwLock::new(agent),
            sidecars,
            jobs: Arc::new(RwLock::new(jobs)),
            processes: Arc::new(RwLock::new(HashMap::new())),
        };
        manager.prune_jobs();
        manager
    }

    pub fn set_agent(&self, agent: AgentConfig) {
        *self.agent.write().expect("agent config lock") = agent;
    }

    /// Stop every agent process group owned by this daemon. The service unit
    /// deliberately uses `KillMode=process` so module sidecars and VMs survive
    /// a restart; agent jobs must not be left running behind that policy.
    pub fn shutdown(&self) {
        let pids: Vec<u32> = self
            .processes
            .read()
            .expect("agent processes lock")
            .values()
            .copied()
            .collect();
        for pid in pids {
            terminate_pid(pid);
        }
    }

    fn agent_snapshot(&self) -> AgentConfig {
        self.agent.read().expect("agent config lock").clone()
    }

    pub fn list(&self) -> Vec<BuildJob> {
        let mut jobs: Vec<_> = self
            .jobs
            .read()
            .expect("build jobs lock")
            .values()
            .cloned()
            .collect();
        jobs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        jobs
    }

    pub fn get(&self, id: &str) -> Option<BuildJob> {
        self.jobs.read().expect("build jobs lock").get(id).cloned()
    }

    fn check_capacity(&self) -> Result<()> {
        let jobs = self.jobs.read().expect("build jobs lock");
        let active = jobs
            .values()
            .filter(|job| matches!(job.status.as_str(), "queued" | "running"))
            .count();
        if active >= MAX_ACTIVE_JOBS {
            anyhow::bail!("too many agent jobs are already active (maximum {MAX_ACTIVE_JOBS})");
        }
        Ok(())
    }

    fn prune_jobs(&self) {
        let mut jobs = self.jobs.write().expect("build jobs lock");
        if jobs.len() < MAX_JOBS {
            return;
        }
        let mut finished = jobs
            .iter()
            .filter(|(_, job)| matches!(job.status.as_str(), "succeeded" | "failed"))
            .map(|(id, job)| (job.started_at.clone(), id.clone()))
            .collect::<Vec<_>>();
        finished.sort();
        let remove = jobs.len().saturating_sub(MAX_JOBS) + 1;
        for (_, id) in finished.into_iter().take(remove) {
            jobs.remove(&id);
            let _ = std::fs::remove_file(self.jobs_dir.join(format!("{id}.json")));
        }
    }

    fn persist(&self, job: &BuildJob) {
        let path = self.jobs_dir.join(format!("{}.json", job.id));
        let write = || -> Result<()> {
            std::fs::create_dir_all(&self.jobs_dir)?;
            write_job(&path, job)
        };
        if let Err(error) = write() {
            warn!(job = %job.id, %error, "could not persist agent job");
        }
    }

    pub fn start(self: &Arc<Self>, modules: Arc<ModuleStore>, prompt: String) -> Result<BuildJob> {
        if prompt.trim().is_empty() {
            anyhow::bail!("describe the module you want");
        }
        self.check_capacity()?;
        self.prune_jobs();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let workspace = self.paths.workshop_dir.join("jobs").join(&id);
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("AGENTS.md"), agent_guidance())?;
        std::fs::write(
            workspace.join("TASK.md"),
            format!(
                "# Module request\n\n{}\n\nCreate the module in this directory. Put `module.toml` at the root and the page under `ui/`.\n",
                prompt.trim()
            ),
        )?;
        let _ = util::init_git_repo(&workspace);
        let job = BuildJob {
            id: id.clone(),
            prompt: prompt.clone(),
            status: "queued".into(),
            error: None,
            module_id: None,
            workspace: workspace.to_string_lossy().to_string(),
            kind: "module".into(),
            started_at: util::now_iso(),
            finished_at: None,
            session_id: None,
        };
        self.jobs
            .write()
            .expect("build jobs lock")
            .insert(id.clone(), job.clone());
        self.persist(&job);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let runner = Arc::clone(&manager);
            if let Err(error) = runner.run(id.clone(), workspace, prompt, modules).await {
                manager.update(&id, |job| {
                    job.status = "failed".into();
                    job.error = Some(format!("{error:#}"));
                    job.finished_at = Some(util::now_iso());
                });
            }
        });
        Ok(job)
    }

    pub fn start_edit(
        self: &Arc<Self>,
        modules: Arc<ModuleStore>,
        module_id: String,
        prompt: String,
    ) -> Result<BuildJob> {
        if prompt.trim().is_empty() {
            anyhow::bail!("describe the modification you want");
        }
        self.check_capacity()?;
        self.prune_jobs();
        let installed = modules
            .get(&module_id)
            .with_context(|| format!("module {module_id} is not installed"))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let workspace = self.paths.workshop_dir.join("jobs").join(&id);
        copy_editable_tree(&installed.root, &workspace)?;
        std::fs::write(workspace.join("AGENTS.md"), agent_guidance())?;
        std::fs::write(
            workspace.join("TASK.md"),
            format!(
                "# Module edit request\n\nModule: {} (`{}`)\n\n{}\n\nModify the existing module in this directory. Keep the module id `{}` and preserve the published contract.\n",
                installed.manifest.name,
                module_id,
                prompt.trim(),
                module_id
            ),
        )?;
        let _ = util::init_git_repo(&workspace);
        let job = BuildJob {
            id: id.clone(),
            prompt: prompt.clone(),
            status: "queued".into(),
            error: None,
            module_id: Some(module_id.clone()),
            workspace: workspace.to_string_lossy().to_string(),
            kind: "edit".into(),
            started_at: util::now_iso(),
            finished_at: None,
            session_id: None,
        };
        self.jobs
            .write()
            .expect("build jobs lock")
            .insert(id.clone(), job.clone());
        self.persist(&job);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let runner = Arc::clone(&manager);
            if let Err(error) = runner
                .run_edit(id.clone(), workspace, prompt, modules, module_id)
                .await
            {
                manager.update(&id, |job| {
                    job.status = "failed".into();
                    job.error = Some(format!("{error:#}"));
                    job.finished_at = Some(util::now_iso());
                });
            }
        });
        Ok(job)
    }

    pub fn start_theme(self: &Arc<Self>, request: ThemeJobRequest) -> Result<BuildJob> {
        if request.prompt.trim().is_empty() {
            anyhow::bail!("describe the theme you want");
        }
        self.check_capacity()?;
        self.prune_jobs();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let workspace = self.paths.workshop_dir.join("jobs").join(&id);
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("base.css"), &request.base_css)?;
        std::fs::write(workspace.join("THEME.md"), include_str!("../docs/THEME.md"))?;
        std::fs::write(workspace.join("AGENTS.md"), theme_guidance())?;
        std::fs::write(
            workspace.join("TASK.md"),
            format!(
                "# Theme request\n\nBase theme: `{}`\n\n{}\n\nCreate `theme.css` and `theme.json` in this directory. This is a new standalone theme for the picker, not an override.\n",
                request.base_theme,
                request.prompt.trim()
            ),
        )?;
        let job = BuildJob {
            id: id.clone(),
            prompt: request.prompt.clone(),
            status: "queued".into(),
            error: None,
            module_id: None,
            workspace: workspace.to_string_lossy().to_string(),
            kind: "theme".into(),
            started_at: util::now_iso(),
            finished_at: None,
            session_id: None,
        };
        self.jobs
            .write()
            .expect("build jobs lock")
            .insert(id.clone(), job.clone());
        self.persist(&job);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let runner = Arc::clone(&manager);
            if let Err(error) = runner.run_theme(id.clone(), workspace, request).await {
                manager.update(&id, |job| {
                    job.status = "failed".into();
                    job.error = Some(format!("{error:#}"));
                    job.finished_at = Some(util::now_iso());
                });
            }
        });
        Ok(job)
    }

    pub fn start_agent(self: &Arc<Self>, prompt: String, root: String) -> Result<BuildJob> {
        if prompt.trim().is_empty() {
            anyhow::bail!("describe what the agent should do");
        }
        self.check_capacity()?;
        self.prune_jobs();
        let root = std::path::PathBuf::from(shellexpand_home(&root));
        let root = root.canonicalize().context("agent root does not exist")?;
        if !root.is_dir() {
            anyhow::bail!("agent root is not a directory");
        }
        if root == std::path::Path::new("/") {
            anyhow::bail!("refusing to run the agent at the filesystem root");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let job = BuildJob {
            id: id.clone(),
            prompt: prompt.clone(),
            status: "queued".into(),
            error: None,
            module_id: None,
            workspace: root.to_string_lossy().to_string(),
            kind: "agent".into(),
            started_at: util::now_iso(),
            finished_at: None,
            session_id: None,
        };
        self.jobs
            .write()
            .expect("build jobs lock")
            .insert(id.clone(), job.clone());
        self.persist(&job);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let runner = Arc::clone(&manager);
            if let Err(error) = runner.run_agent(id.clone(), root, prompt).await {
                manager.update(&id, |job| {
                    job.status = "failed".into();
                    job.error = Some(format!("{error:#}"));
                    job.finished_at = Some(util::now_iso());
                });
            }
        });
        Ok(job)
    }

    async fn run_agent(
        self: Arc<Self>,
        id: String,
        root: std::path::PathBuf,
        prompt: String,
    ) -> Result<()> {
        self.update(&id, |job| {
            job.status = "running".into();
        });
        let agent = self.agent_snapshot();
        self.clone()
            .execute_agent(id.clone(), agent, prompt, root)
            .await?;
        self.update(&id, |job| {
            job.status = "succeeded".into();
            job.finished_at = Some(util::now_iso());
        });
        Ok(())
    }

    async fn run(
        self: Arc<Self>,
        id: String,
        workspace: std::path::PathBuf,
        prompt: String,
        modules: Arc<ModuleStore>,
    ) -> Result<()> {
        self.update(&id, |job| {
            job.status = "running".into();
        });
        let agent = self.agent_snapshot();
        let build_prompt = module_build_prompt(&prompt);
        self.clone()
            .execute_agent(id.clone(), agent, build_prompt, workspace.clone())
            .await?;
        util::git_commit(&workspace, "Orqella agent module");
        let installed = modules
            .install_from_path(&workspace)
            .context("agent finished, but no valid module was produced")?;
        self.update(&id, |job| {
            job.status = "succeeded".into();
            job.module_id = Some(installed.manifest.id.clone());
            job.finished_at = Some(util::now_iso());
        });
        Ok(())
    }

    async fn run_edit(
        self: Arc<Self>,
        id: String,
        workspace: std::path::PathBuf,
        prompt: String,
        modules: Arc<ModuleStore>,
        module_id: String,
    ) -> Result<()> {
        self.update(&id, |job| {
            job.status = "running".into();
        });
        let agent = self.agent_snapshot();
        let edit_prompt = module_edit_prompt(&module_id, &prompt);
        self.clone()
            .execute_agent(id.clone(), agent, edit_prompt, workspace.clone())
            .await?;
        let edited = Manifest::load(&workspace)
            .context("agent finished, but no valid module was produced")?;
        if edited.id != module_id {
            anyhow::bail!(
                "agent changed the module id from `{module_id}` to `{}`; the edit was not installed",
                edited.id
            );
        }
        if let Some(current) = modules.get(&module_id) {
            copy_git_metadata(&current.root, &workspace)?;
        }
        util::git_commit(&workspace, "Orqella agent module edit");
        let was_running = self.sidecars.is_running(&module_id);
        let installed = modules
            .install_from_path_preserving_git(&workspace)
            .context("agent finished, but the edited module could not be installed")?;
        if was_running {
            self.sidecars.stop(&module_id);
            if let Some(module) = modules.get(&module_id)
                && let Err(error) = self.sidecars.start(&module).await
            {
                warn!(
                    module = %module_id,
                    "module installed, but its sidecar did not restart: {error:#}"
                );
            }
        }
        self.update(&id, |job| {
            job.status = "succeeded".into();
            job.module_id = Some(installed.manifest.id.clone());
            job.finished_at = Some(util::now_iso());
        });
        Ok(())
    }

    async fn run_theme(
        self: Arc<Self>,
        id: String,
        workspace: PathBuf,
        request: ThemeJobRequest,
    ) -> Result<()> {
        self.update(&id, |job| {
            job.status = "running".into();
        });
        let agent = self.agent_snapshot();
        let theme_prompt = format!(
            "Create a new standalone Orqella dashboard theme based on `{}` for this request:\n\n{}\n\nRead AGENTS.md, THEME.md, and base.css. Write `theme.css` with the complete token set and `theme.json` with the theme name, description, and color_scheme. Do not modify anything outside this directory.",
            request.base_theme, request.prompt
        );
        self.clone()
            .execute_agent(id.clone(), agent, theme_prompt, workspace.clone())
            .await?;
        let generated = std::fs::read_to_string(workspace.join("theme.css"))
            .context("agent finished, but did not create theme.css")?;
        let css = validate_generated_theme(&generated)?;
        let meta = std::fs::read_to_string(workspace.join("theme.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<GeneratedThemeMeta>(&raw).ok())
            .unwrap_or_default();
        let name = clean_theme_text(&meta.name, &request.fallback_name, 48);
        let description = clean_theme_text(&meta.description, &request.fallback_description, 200);
        let requested_scheme = meta.color_scheme.trim().to_ascii_lowercase();
        let color_scheme = match requested_scheme.as_str() {
            "light" => "light",
            "dark" => "dark",
            _ => request.base_scheme.as_str(),
        };
        theme::install_custom(
            &self.paths.config_dir,
            &request.theme_id,
            &name,
            &description,
            color_scheme,
            &css,
        )?;
        self.update(&id, |job| {
            job.status = "succeeded".into();
            job.finished_at = Some(util::now_iso());
        });
        Ok(())
    }

    /// Run the configured harness in the job workspace and watch its output
    /// for a conversation id. `timeout_secs = 0` leaves the agent running
    /// until it exits; a positive value stops the whole process group.
    async fn execute_agent(
        self: Arc<Self>,
        id: String,
        agent: AgentConfig,
        prompt: String,
        workspace: PathBuf,
    ) -> Result<std::process::ExitStatus> {
        // Soft hardening only: the harness uses the user's existing config and
        // credentials. A real filesystem boundary requires an OS sandbox; a
        // fake HOME alone does not stop absolute-path reads.
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut command = Command::new(&agent.command);
        command
            .args(prompt_args(&agent, &prompt))
            // The agent is a code-execution boundary of its own. Do not leak the
            // daemon's environment (cloud keys, SSH agent sockets, service
            // credentials) into a harness that may be prompt-injected.
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("LANG", "C.UTF-8")
            .env("TMPDIR", std::env::temp_dir())
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .envs(agent.resolved_env())
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        for key in [
            "SystemRoot",
            "WINDIR",
            "SystemDrive",
            "COMSPEC",
            "PATHEXT",
            "ProgramData",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "CommonProgramFiles",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        #[cfg(not(windows))]
        for key in [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            command.creation_flags(0x00000008 | 0x00000200);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start agent command `{}`", agent.command))?;
        if let Some(pid) = child.id() {
            self.processes
                .write()
                .expect("agent processes lock")
                .insert(id.clone(), pid);
        }
        let _process_entry = ProcessEntry {
            manager: Arc::clone(&self),
            id: id.clone(),
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let watch_out = Arc::clone(&self);
        let watch_out_id = id.clone();
        let stdout_task = tokio::spawn(async move {
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    watch_out.observe_output(&watch_out_id, &line);
                }
            }
        });
        let watch_err = Arc::clone(&self);
        let watch_err_id = id.clone();
        let stderr_task = tokio::spawn(async move {
            if let Some(stderr) = stderr {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    watch_err.observe_output(&watch_err_id, &line);
                }
            }
        });
        let status = match timeout_after(agent.timeout_secs) {
            None => child.wait().await.context("agent process failed")?,
            Some(timeout) => match tokio::time::timeout(timeout, child.wait()).await {
                Ok(status) => status.context("agent process failed")?,
                Err(_) => {
                    terminate_tree(&mut child).await;
                    let _ = child.wait().await;
                    // Do not wait forever for readers held open by a
                    // grandchild that escaped the process group.
                    stdout_task.abort();
                    stderr_task.abort();
                    anyhow::bail!(
                        "agent timed out after {} seconds; the harness session may still be running",
                        timeout.as_secs()
                    );
                }
            },
        };
        // A harness can exit while a detached child keeps the pipe open. Bound
        // the drain so the job cannot hang after the agent has finished.
        let mut stdout_task = stdout_task;
        let mut stderr_task = stderr_task;
        for task in [&mut stdout_task, &mut stderr_task] {
            if tokio::time::timeout(Duration::from_secs(5), &mut *task)
                .await
                .is_err()
            {
                task.abort();
                warn!(job = %id, "agent output pipe stayed open after the agent exited");
            }
        }
        if !status.success() {
            anyhow::bail!("agent exited with {status}");
        }
        Ok(status)
    }

    pub async fn wait_for_session(&self, id: &str, wait: std::time::Duration) -> Option<BuildJob> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let job = self.get(id)?;
            if job.session_id.is_some()
                || matches!(job.status.as_str(), "failed" | "succeeded")
                || tokio::time::Instant::now() >= deadline
            {
                return Some(job);
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut BuildJob)) {
        let job = {
            let mut jobs = self.jobs.write().expect("build jobs lock");
            let Some(job) = jobs.get_mut(id) else {
                return;
            };
            change(job);
            job.clone()
        };
        self.persist(&job);
    }

    fn observe_output(&self, id: &str, line: &str) {
        let Some(session) = session_id_from_line(line) else {
            return;
        };
        let mut new_session = false;
        {
            let mut jobs = self.jobs.write().expect("build jobs lock");
            let Some(job) = jobs.get_mut(id) else {
                return;
            };
            if job.session_id.as_deref() != Some(session.as_str()) {
                job.session_id = Some(session);
                new_session = true;
            }
        }
        if new_session && let Some(job) = self.get(id) {
            self.persist(&job);
        }
    }
}

fn copy_git_metadata(installed: &Path, workspace: &Path) -> Result<()> {
    let source = installed.join(".git");
    if !source.is_dir() {
        return Ok(());
    }
    let target = workspace.join(".git");
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    copy_tree(&source, &target)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn copy_editable_tree(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_editable_tree_inner(src, dst, src)
}

fn copy_editable_tree_inner(src: &Path, dst: &Path, root: &Path) -> Result<()> {
    let at_root = src == root;
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("could not read module source {}", src.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if (at_root
            && (name_str.eq_ignore_ascii_case("data")
                || matches!(
                    name_str.as_ref(),
                    ".config" | ".cache" | ".local" | "sidecar.log"
                )))
            || matches!(
                name_str.as_ref(),
                ".git" | "target" | "node_modules" | ".venv" | "__pycache__"
            )
        {
            continue;
        }
        let target = dst.join(name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_editable_tree_inner(&entry.path(), &target, root)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn shellexpand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| value.to_string());
    }
    value.to_string()
}

fn session_id_from_line(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    for key in ["sessionID", "session_id", "thread_id", "conversation_id"] {
        if let Some(id) = value.get(key).and_then(Value::as_str)
            && !id.trim().is_empty()
        {
            return Some(id.trim().to_string());
        }
    }
    value
        .get("session")
        .and_then(|session| session.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string())
}

fn timeout_after(seconds: u64) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

fn write_job(path: &Path, job: &BuildJob) -> Result<()> {
    let data = serde_json::to_vec_pretty(job)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, data)?;
    let _ = util::restrict_file(&temp);
    std::fs::rename(&temp, path)?;
    Ok(())
}

fn load_jobs(dir: &Path) -> HashMap<String, BuildJob> {
    let mut jobs = HashMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return jobs,
    };
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(mut job) = serde_json::from_slice::<BuildJob>(&data) else {
            continue;
        };
        let had_log = serde_json::from_slice::<Value>(&data)
            .ok()
            .and_then(|value| value.get("log").cloned())
            .is_some();
        let was_active = matches!(job.status.as_str(), "queued" | "running");
        if was_active {
            job.status = "failed".into();
            job.error = Some(
                "Orqella restarted before this agent job finished; the harness session may still be running"
                    .into(),
            );
            job.finished_at = Some(util::now_iso());
        }
        jobs.insert(job.id.clone(), job.clone());
        // Rewrite old job files so agent output is not retained on disk.
        if was_active || had_log {
            let _ = write_job(&entry.path(), &job);
        }
    }
    jobs
}

struct ProcessEntry {
    manager: Arc<BuildManager>,
    id: String,
}

impl Drop for ProcessEntry {
    fn drop(&mut self) {
        self.manager
            .processes
            .write()
            .expect("agent processes lock")
            .remove(&self.id);
    }
}

#[cfg(unix)]
fn terminate_pid(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn terminate_pid(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
async fn terminate_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        // The agent was started as its own process-group leader, so this
        // reaches its tool subprocesses too.
        terminate_pid(pid);
    }
    let _ = child.start_kill();
}

#[cfg(windows)]
async fn terminate_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        terminate_pid(pid);
    }
    let _ = child.kill().await;
}

fn agent_guidance() -> &'static str {
    include_str!("../docs/MODULE_AGENT.md")
}

fn module_build_prompt(prompt: &str) -> String {
    format!("Build an Orqella dashboard module for this request:\n\n{prompt}")
}

fn module_edit_prompt(module_id: &str, prompt: &str) -> String {
    format!(
        "Edit the existing Orqella dashboard module `{module_id}` for this request:\n\n{prompt}"
    )
}

fn theme_guidance() -> &'static str {
    r#"# Orqella theme task

Create a complete, standalone Orqella dashboard theme in this directory.

- Read `base.css` for the current theme and `THEME.md` for the token contract.
- Write `theme.css` as a complete theme, not a partial override. Start from `base.css` and keep every token definition, replacing values to match the request.
- Write `theme.json` with this shape:
  `{ "name": "Theme name", "description": "One sentence.", "color_scheme": "dark" }`
  Use `light` or `dark` for `color_scheme`.
- Keep the interface usable: readable text contrast, visible focus rings, and clear hover, active, warning, and error states.
- Use only CSS. Do not use `@import`, `@font-face`, `url()`, JavaScript, or remote assets.
- Do not modify anything outside this directory.
- The theme must be non-empty, stay under 256 KiB, contain a `:root` rule, and define the core tokens shown in `base.css`.
"#
}

#[derive(Debug, Default, Deserialize)]
struct GeneratedThemeMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color_scheme: String,
}

fn clean_theme_text(value: &str, fallback: &str, max_chars: usize) -> String {
    let value = if value.trim().is_empty() {
        fallback
    } else {
        value
    };
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn validate_generated_theme(raw: &str) -> Result<String> {
    const MAX_THEME_BYTES: usize = 256 * 1024;
    const REQUIRED_TOKENS: &[&str] = &[
        "--bg",
        "--panel",
        "--line",
        "--ink",
        "--muted",
        "--mint",
        "--radius",
        "--font-sans",
        "--font-mono",
        "--shadow",
    ];
    let css = raw.trim();
    if css.is_empty() {
        anyhow::bail!("generated theme.css is empty");
    }
    if css.len() > MAX_THEME_BYTES {
        anyhow::bail!("generated theme.css is larger than 256 KiB");
    }
    if css.as_bytes().contains(&0) {
        anyhow::bail!("generated theme.css contains binary data");
    }
    let lower = css.to_ascii_lowercase();
    if !lower.contains(":root") {
        anyhow::bail!("generated theme.css has no :root rule");
    }
    for token in REQUIRED_TOKENS {
        if !lower.contains(token) {
            anyhow::bail!("generated theme.css is missing the {token} token");
        }
    }
    for forbidden in ["@import", "@font-face", "url("] {
        if lower.contains(forbidden) {
            anyhow::bail!("generated theme.css contains unsupported {forbidden}");
        }
    }
    Ok(css.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_appended_by_default() {
        let agent = AgentConfig {
            args: vec!["-p".into(), "--output-format".into(), "text".into()],
            ..AgentConfig::default()
        };
        assert_eq!(
            prompt_args(&agent, "hello"),
            vec!["-p", "--output-format", "text", "hello"]
        );
    }

    #[test]
    fn parses_session_ids_from_agent_events() {
        assert_eq!(
            session_id_from_line(r#"{"type":"step_start","sessionID":"ses_123"}"#),
            Some("ses_123".to_string())
        );
        assert_eq!(
            session_id_from_line(r#"{"thread_id":"thread-123"}"#),
            Some("thread-123".to_string())
        );
        assert_eq!(session_id_from_line("plain log line"), None);
    }

    #[test]
    fn prompt_placeholder_is_substituted() {
        let agent = AgentConfig {
            args: vec!["--prompt".into(), "{prompt}".into(), "--json".into()],
            ..AgentConfig::default()
        };
        assert_eq!(
            prompt_args(&agent, "hello"),
            vec!["--prompt", "hello", "--json"]
        );
    }

    #[test]
    fn module_prompts_keep_the_request_as_the_task() {
        let build = module_build_prompt("show my CPU");
        assert_eq!(
            build,
            "Build an Orqella dashboard module for this request:\n\nshow my CPU"
        );
        let edit = module_edit_prompt("com.example.demo", "add a chart");
        assert_eq!(
            edit,
            "Edit the existing Orqella dashboard module `com.example.demo` for this request:\n\nadd a chart"
        );
        for prompt in [build, edit] {
            let lower = prompt.to_ascii_lowercase();
            assert!(!lower.contains("untrusted"));
            assert!(!lower.contains("do not modify anything outside"));
        }
    }

    #[test]
    fn zero_timeout_means_no_timeout() {
        assert_eq!(timeout_after(0), None);
        assert_eq!(timeout_after(900), Some(Duration::from_secs(900)));
    }

    #[test]
    fn loads_persisted_jobs_and_marks_interrupted_work_failed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let running = BuildJob {
            id: "running-job".into(),
            prompt: "build it".into(),
            status: "running".into(),
            error: None,
            module_id: None,
            workspace: "/tmp/workspace".into(),
            kind: "module".into(),
            started_at: "2026-11-11T20:00:00.000Z".into(),
            finished_at: None,
            session_id: Some("ses_123".into()),
        };
        let finished = BuildJob {
            id: "finished-job".into(),
            status: "succeeded".into(),
            error: None,
            finished_at: Some("2026-11-11T20:10:00.000Z".into()),
            ..running.clone()
        };
        std::fs::write(
            dir.path().join("running-job.json"),
            serde_json::to_vec(&{
                let mut value = serde_json::to_value(&running).expect("serialize");
                value["log"] = serde_json::json!("legacy agent output");
                value
            })
            .expect("serialize"),
        )
        .expect("write");
        std::fs::write(
            dir.path().join("finished-job.json"),
            serde_json::to_vec(&finished).expect("serialize"),
        )
        .expect("write");
        let jobs = load_jobs(dir.path());
        assert_eq!(jobs.len(), 2);
        let restored = jobs.get("running-job").expect("running job");
        assert_eq!(restored.status, "failed");
        assert_eq!(restored.session_id.as_deref(), Some("ses_123"));
        assert!(
            restored
                .error
                .as_deref()
                .unwrap_or("")
                .contains("restarted")
        );
        let rewritten = std::fs::read(dir.path().join("running-job.json")).expect("read");
        let rewritten: Value = serde_json::from_slice(&rewritten).expect("json");
        assert!(rewritten.get("log").is_none());
        assert_eq!(rewritten["status"], "failed");
        assert_eq!(
            jobs.get("finished-job").expect("finished job").status,
            "succeeded"
        );
    }

    #[test]
    fn git_metadata_is_copied_into_an_edited_workspace() {
        let installed = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(installed.path().join(".git/objects")).unwrap();
        std::fs::write(installed.path().join(".git/config"), "git").unwrap();
        std::fs::write(installed.path().join(".git/objects/blob"), "object").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        copy_git_metadata(installed.path(), workspace.path()).unwrap();
        assert!(workspace.path().join(".git/config").is_file());
        assert!(workspace.path().join(".git/objects/blob").is_file());

        // A workspace without source metadata stays untouched.
        let plain = tempfile::tempdir().unwrap();
        copy_git_metadata(plain.path(), workspace.path()).unwrap();
        assert!(workspace.path().join(".git/config").is_file());
    }

    #[test]
    fn editable_copy_excludes_runtime_and_git_data() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join(".git")).unwrap();
        std::fs::create_dir_all(source.path().join("data")).unwrap();
        std::fs::create_dir_all(source.path().join("ui/data")).unwrap();
        std::fs::create_dir_all(source.path().join("target")).unwrap();
        std::fs::create_dir_all(source.path().join("backend/__pycache__")).unwrap();
        std::fs::write(source.path().join(".git/config"), "git").unwrap();
        std::fs::write(source.path().join("data/secret"), "runtime").unwrap();
        std::fs::write(source.path().join("ui/data/presets.json"), "[]").unwrap();
        std::fs::write(source.path().join("target/build"), "build").unwrap();
        std::fs::write(source.path().join("module.toml"), "schema = 1").unwrap();
        let destination = tempfile::tempdir().unwrap();
        copy_editable_tree(source.path(), destination.path()).unwrap();
        assert!(destination.path().join("module.toml").is_file());
        assert!(destination.path().join("ui/data/presets.json").is_file());
        for skipped in [
            ".git/config",
            "data/secret",
            "target/build",
            "backend/__pycache__",
        ] {
            assert!(
                !destination.path().join(skipped).exists(),
                "{skipped} should not be copied"
            );
        }
    }

    const THEME_CSS: &str = r#":root {
        --bg: #101018;
        --panel: #181822;
        --line: #303040;
        --ink: #f0f0f8;
        --muted: #a0a0b0;
        --mint: #88aaff;
        --radius: 8px;
        --font-sans: sans-serif;
        --font-mono: monospace;
        --shadow: 0 0 1px #000;
    }"#;

    #[test]
    fn generated_themes_are_validated() {
        assert_eq!(validate_generated_theme(THEME_CSS).unwrap(), THEME_CSS);
        for invalid in [
            "",
            "body { color: red; }",
            ":root { --bg: #101018; }",
            r#":root { --bg: url(https://example.com/bg.png); }"#,
            r#":root { } @import 'https://example.com/theme.css';"#,
            r#":root { } @font-face { font-family: demo; }"#,
        ] {
            assert!(
                validate_generated_theme(invalid).is_err(),
                "{invalid} should be rejected"
            );
        }
        let oversized = format!("{THEME_CSS}\n/* {} */", "x".repeat(256 * 1024));
        assert!(validate_generated_theme(&oversized).is_err());
    }

    #[test]
    fn theme_metadata_uses_defaults_for_missing_fields() {
        assert_eq!(clean_theme_text("", "fallback", 12), "fallback");
        assert_eq!(clean_theme_text("  named  ", "fallback", 12), "named");
        assert_eq!(
            clean_theme_text("a theme with a very long name", "fallback", 8),
            "a theme"
        );
    }

    #[test]
    fn reads_session_ids_from_plain_and_nested_objects() {
        assert_eq!(
            session_id_from_line(r#"{"session":{"id":"ses_nested"}}"#),
            Some("ses_nested".to_string())
        );
        assert_eq!(
            session_id_from_line(r#"{"session_id":"ses_snake"}"#),
            Some("ses_snake".to_string())
        );
        assert_eq!(
            session_id_from_line(r#"{"conversation_id":"conv_1"}"#),
            Some("conv_1".to_string())
        );
        assert_eq!(session_id_from_line(r#"{"id":"not-a-session"}"#), None);
        assert_eq!(session_id_from_line(r#"{"session_id":"   "}"#), None);
        assert_eq!(session_id_from_line(""), None);
    }
}
