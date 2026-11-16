use crate::config::SidecarPolicy;
#[cfg(any(windows, target_os = "macos"))]
use crate::modules::containment;
use crate::modules::manifest::{BackendArtifact, BackendKind, Manifest, host_platform};
use crate::modules::store::InstalledModule;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::Method;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;

const RETRY_BASE_SECS: u64 = 8;
const RETRY_MAX_SECS: u64 = 300;
const FAILED_AFTER_ATTEMPTS: u32 = 5;
const ARTIFACT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const ARTIFACT_TIMEOUT_SECS: u64 = 300;

/// Directories a sidecar may have created through `$HOME` while HOME was the
/// module root. They are runtime state, not source, and move under `data/home`
/// once. Keep this list to conventional home directories.
const LEGACY_HOME_DIRS: &[&str] = &[
    ".config",
    ".cache",
    ".local",
    ".homeassistant",
    ".npm",
    ".bun",
    ".cargo",
    ".gradle",
    ".m2",
    ".lmstudio",
    ".ollama",
    ".vscode",
    ".vscode-server",
    ".gem",
    ".terraform.d",
    ".docker",
    ".aws",
    ".gnupg",
    ".ssh",
];

/// Return the sidecar home under the module data directory, migrating legacy
/// home directories out of the module root on first use. The marker means a
/// failed migration is retried on the next start instead of being forgotten.
fn prepare_module_home(root: &Path) -> Result<PathBuf> {
    let data = crate::modules::ensure_module_data_dir(root)?;
    let home = data.join("home");
    match std::fs::symlink_metadata(&home) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("module home directory is a symlink");
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!("module home path is not a directory");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&home)?;
        }
        Err(error) => return Err(error.into()),
    }
    crate::util::restrict_dir(&home)?;
    let marker = home.join(".orqella-home-v1");
    if marker.exists() {
        return Ok(home);
    }

    let mut ls_args = vec!["ls-files", "--"];
    ls_args.extend(LEGACY_HOME_DIRS.iter().copied());
    let tracked_output = crate::modules::git::run_allow_failure(root, &ls_args).unwrap_or_default();
    let tracked_roots: std::collections::HashSet<&str> = tracked_output
        .lines()
        .map(|line| line.split('/').next().unwrap_or(line))
        .collect();

    let mut complete = true;
    for name in LEGACY_HOME_DIRS {
        let source = root.join(name);
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %source.display(), %error, "could not inspect legacy sidecar home");
                complete = false;
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        if tracked_roots.contains(name) {
            // A checked-in dot-directory is module source, not runtime state.
            continue;
        }
        let destination = home.join(name);
        if destination.exists() {
            continue;
        }
        match std::fs::rename(&source, &destination) {
            Ok(()) => tracing::info!(
                module_root = %root.display(),
                directory = name,
                "moved legacy sidecar home into data/home"
            ),
            Err(error) => {
                tracing::warn!(
                    source = %source.display(),
                    destination = %destination.display(),
                    %error,
                    "could not migrate legacy sidecar home"
                );
                complete = false;
            }
        }
    }
    if complete {
        crate::util::write_atomic(&marker, b"1\n")?;
    }
    Ok(home)
}

/// Whether this host can actually put a sidecar in an OS sandbox. Windows has
/// no filesystem containment today, so every Windows sidecar is effective
/// unsandboxed even when the manifest's default is `true`.
pub fn sandbox_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        command_exists("bwrap")
    }
    #[cfg(target_os = "macos")]
    {
        command_exists("sandbox-exec")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

pub fn effective_sandbox(policy: &SidecarPolicy, manifest: &Manifest) -> bool {
    policy.sandbox && manifest.backend.sandbox.unwrap_or(true) && sandbox_available()
}

pub fn effective_sandbox_for_manifest(manifest: &Manifest) -> bool {
    manifest.backend.sandbox.unwrap_or(true) && sandbox_available()
}

struct Sidecar {
    child: Child,
    port: u16,
    token: String,
    log_path: PathBuf,
    #[cfg(windows)]
    _job: Option<containment::JobGuard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SidecarStatus {
    pub state: String,
    pub attempts: u32,
    pub last_error: String,
    pub next_retry_in_secs: u64,
}

struct SidecarFailure {
    attempts: u32,
    last_error: String,
    next_attempt: Instant,
}

pub struct SidecarManager {
    processes: Mutex<HashMap<String, Sidecar>>,
    failures: Mutex<HashMap<String, SidecarFailure>>,
    client: reqwest::Client,
    artifact_client: reqwest::Client,
    start_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    policy: SidecarPolicy,
    sandbox_dir: PathBuf,
    logs_dir: PathBuf,
    venvs_dir: PathBuf,
    protected_paths: Vec<PathBuf>,
}

impl Default for SidecarManager {
    fn default() -> Self {
        let root = std::env::temp_dir().join("orqella");
        Self::new(
            SidecarPolicy::default(),
            root.clone(),
            root.join("logs"),
            root.join("venvs"),
            Vec::new(),
        )
    }
}

impl SidecarManager {
    pub fn effective_sandbox(&self, module: &InstalledModule) -> bool {
        effective_sandbox(&self.policy, &module.manifest)
    }

    pub fn new(
        policy: SidecarPolicy,
        sandbox_dir: PathBuf,
        logs_dir: PathBuf,
        venvs_dir: PathBuf,
        protected_paths: Vec<PathBuf>,
    ) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            artifact_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(5))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            start_locks: Mutex::new(HashMap::new()),
            policy,
            sandbox_dir,
            logs_dir,
            venvs_dir,
            protected_paths,
        }
    }

    /// Daemon-owned sidecar log path. Keeping logs outside the module root
    /// prevents a module from turning `sidecar.log` into a symlink or hardlink
    /// that the daemon would follow with its own privileges.
    pub fn log_path(&self, id: &str) -> PathBuf {
        self.logs_dir.join(format!("{id}.log"))
    }

    fn start_lock_for(&self, id: &str) -> Arc<AsyncMutex<()>> {
        self.start_locks
            .lock()
            .expect("sidecar start lock")
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn protected_read_path(&self, path: &str) -> bool {
        let expanded = expand_read_path(path);
        self.protected_paths.iter().any(|protected| {
            expanded == *protected
                || expanded.starts_with(protected)
                || protected.starts_with(&expanded)
        })
    }

    async fn sidecar_command(&self, module: &InstalledModule) -> Result<(String, Vec<String>)> {
        for path in &module.manifest.backend.read_paths {
            if self.protected_read_path(path) {
                bail!(
                    "module `{}` requests a protected read path `{path}`",
                    module.manifest.id
                );
            }
        }
        if let Some(artifact) = module.manifest.backend.artifact_for_current_platform() {
            let path = self.ensure_artifact(module, artifact).await?;
            return Ok((path.to_string_lossy().to_string(), artifact.args.clone()));
        }
        let args = module.manifest.backend.command.clone();
        if args.is_empty() {
            bail!(
                "no sidecar artifact matches {} and backend.command is empty",
                host_platform()
            );
        }
        let is_python = args
            .first()
            .is_some_and(|program| matches!(program.as_str(), "python" | "python3"));
        if module.manifest.backend.python.is_some()
            || module.manifest.backend.requirements.is_some()
            || is_python
        {
            let setup = PythonSetup {
                module_id: module.manifest.id.clone(),
                root: module.root.clone(),
                venv_root: self.venvs_dir.join(&module.manifest.id),
                python: module.manifest.backend.python.clone(),
                requirements: module.manifest.backend.requirements.clone(),
                command: args,
                sandbox: self.policy.sandbox && module.manifest.backend.sandbox.unwrap_or(true),
                sandbox_dir: self.sandbox_dir.clone(),
                max_memory_mb: self.policy.max_memory_mb,
                read_paths: module.manifest.backend.read_paths.clone(),
            };
            let (program, args) = tokio::task::spawn_blocking(move || prepare_python(&setup))
                .await
                .context("python setup worker failed")??;
            return Ok((program, args));
        }
        Ok(resolve_sidecar_command(&module.root, &args))
    }

    async fn ensure_artifact(
        &self,
        module: &InstalledModule,
        artifact: &BackendArtifact,
    ) -> Result<PathBuf> {
        let digest = artifact.sha256.trim().to_ascii_lowercase();
        if !artifact.path.trim().is_empty() {
            let relative = Path::new(artifact.path.trim());
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                bail!(
                    "backend artifact path for {} must be relative to the module root",
                    artifact.platform
                );
            }
            let path = module.root.join(relative);
            ensure_regular_file_beneath(&path, &module.root).with_context(|| {
                format!(
                    "backend artifact file for {} is not a regular file inside the module root: {}",
                    artifact.platform,
                    artifact.path.trim()
                )
            })?;
            let executable = path.clone();
            let actual = tokio::task::spawn_blocking(move || file_sha256(&executable))
                .await
                .context("sidecar artifact hash worker failed")??;
            if !actual.eq_ignore_ascii_case(&digest) {
                bail!(
                    "backend artifact checksum mismatch for {}: expected {}, got {}",
                    artifact.platform,
                    digest,
                    actual
                );
            }
            set_artifact_executable(&path)?;
            return Ok(path);
        }

        let directory =
            crate::modules::ensure_module_data_dir(&module.root)?.join(".sidecar-artifacts");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        let extension = if cfg!(windows) { ".exe" } else { "" };
        let path = directory.join(format!("{digest}{extension}"));

        if path.is_file() {
            ensure_regular_file_beneath(&path, &directory)?;
            let cached = path.clone();
            let actual = tokio::task::spawn_blocking(move || file_sha256(&cached))
                .await
                .context("sidecar artifact hash worker failed")?;
            if matches!(&actual, Ok(actual) if actual.eq_ignore_ascii_case(&digest)) {
                set_artifact_executable(&path)?;
                return Ok(path);
            }
            tracing::warn!(
                module = %module.manifest.id,
                path = %path.display(),
                "cached sidecar artifact failed its checksum; downloading again"
            );
            let _ = std::fs::remove_file(&path);
        }

        let digest_prefix = digest.get(..16).unwrap_or(&digest);
        let temporary = directory.join(format!(
            ".{digest_prefix}.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let downloaded = self.download_artifact(artifact, &temporary).await;
        if let Err(error) = downloaded {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        if let Err(error) = set_artifact_executable(&temporary) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| format!("could not store {}", path.display()));
        }
        tracing::info!(
            module = %module.manifest.id,
            platform = %artifact.platform,
            url = %artifact.url,
            "downloaded sidecar artifact"
        );
        Ok(path)
    }

    async fn download_artifact(&self, artifact: &BackendArtifact, path: &Path) -> Result<()> {
        let response = self
            .artifact_client
            .get(artifact.url.trim())
            .header(
                reqwest::header::USER_AGENT,
                concat!("orqella/", env!("CARGO_PKG_VERSION")),
            )
            .timeout(Duration::from_secs(ARTIFACT_TIMEOUT_SECS))
            .send()
            .await
            .with_context(|| {
                format!(
                    "could not download the sidecar artifact for {}",
                    artifact.platform
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "sidecar artifact download for {} returned {status}",
                artifact.platform
            );
        }
        let mut file = tokio::fs::File::create(path)
            .await
            .with_context(|| format!("could not create {}", path.display()))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| {
                format!(
                    "sidecar artifact download for {} was interrupted",
                    artifact.platform
                )
            })?;
            total = total.saturating_add(chunk.len() as u64);
            if total > ARTIFACT_MAX_BYTES {
                bail!(
                    "sidecar artifact for {} exceeds the {} MB limit",
                    artifact.platform,
                    ARTIFACT_MAX_BYTES / (1024 * 1024)
                );
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .with_context(|| format!("could not write sidecar artifact {}", path.display()))?;
        }
        file.flush().await?;
        file.sync_all().await?;
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(artifact.sha256.trim()) {
            bail!(
                "sidecar artifact for {} failed SHA-256 verification",
                artifact.platform
            );
        }
        Ok(())
    }

    pub async fn start(&self, module: &InstalledModule) -> Result<()> {
        if module.manifest.backend.kind != BackendKind::Sidecar {
            return Ok(());
        }
        if !module.manifest.backend.supported_here() {
            return Ok(());
        }
        if !module.grants("backend") {
            bail!(
                "module `{}` has not been granted the backend permission",
                module.manifest.id
            );
        }
        crate::modules::ensure_module_data_dir(&module.root)?;
        let home = prepare_module_home(&module.root)?;
        let start_lock = self.start_lock_for(&module.manifest.id);
        let _start_guard = start_lock.lock().await;
        if self.is_running(&module.manifest.id) {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if self.policy.sandbox && !command_exists("bwrap") {
            bail!(
                "sidecar sandbox is enabled but bwrap is not installed; install bubblewrap or set [sidecar] sandbox = false"
            );
        }
        #[cfg(target_os = "macos")]
        if self.policy.sandbox && !command_exists("sandbox-exec") {
            bail!("sidecar sandbox is enabled but sandbox-exec is not available");
        }
        let port = crate::util::free_port()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let log_path = self.log_path(&module.manifest.id);
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let log = crate::util::open_truncate_no_follow(&log_path)
            .with_context(|| format!("could not open {}", log_path.display()))?;
        let _ = crate::util::restrict_file(&log_path);
        let log_err = log.try_clone()?;

        let (executable, executable_args) = self.sidecar_command(module).await?;
        let sandbox = effective_sandbox(&self.policy, &module.manifest);
        for path in &module.manifest.backend.read_paths {
            if self.protected_read_path(path) {
                bail!(
                    "module `{}` requests a protected read path `{path}`",
                    module.manifest.id
                );
            }
        }
        let mut read_paths = module.manifest.backend.read_paths.clone();
        let venv = self.venvs_dir.join(&module.manifest.id);
        if venv.is_dir() {
            read_paths.push(venv.to_string_lossy().to_string());
        }
        if let Some(requirements) = &module.manifest.backend.requirements {
            let root = module.root.canonicalize().ok();
            let path = module
                .root
                .join(requirements)
                .canonicalize()
                .ok()
                .filter(|path| root.as_ref().is_some_and(|root| path.starts_with(root)));
            if let Some(path) = path.filter(|path| path.is_file()) {
                read_paths.push(path.to_string_lossy().to_string());
            }
        }
        let (program, program_args) = sandboxed_command(
            &module.root,
            &self.sandbox_dir,
            &module.manifest.id,
            sandbox,
            module.manifest.backend.network,
            &executable,
            &executable_args,
            self.policy.max_memory_mb,
            &read_paths,
            &[],
        )?;
        let mut command = Command::new(program);
        command
            .args(program_args)
            .current_dir(&module.root)
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("TMPDIR", "/tmp")
            .env("LANG", "C.UTF-8")
            .env("ORQELLA_MODULE_ID", &module.manifest.id)
            .env("ORQELLA_MODULE_PORT", port.to_string())
            .env("ORQELLA_MODULE_TOKEN", &token)
            .env("ORQELLA_NODE", "local");
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
        #[cfg(windows)]
        {
            let temp = home.join("tmp");
            std::fs::create_dir_all(&temp)?;
            command.env("TEMP", &temp).env("TMP", &temp);
        }
        if !sandbox {
            for key in [
                "DBUS_SESSION_BUS_ADDRESS",
                "XDG_RUNTIME_DIR",
                "WAYLAND_DISPLAY",
                "DISPLAY",
                "XAUTHORITY",
                "HYPRLAND_INSTANCE_SIGNATURE",
                "XDG_SESSION_TYPE",
            ] {
                if let Ok(value) = std::env::var(key) {
                    command.env(key, value);
                }
            }
        }
        command
            .env(
                "ORQELLA_HOST_HOME",
                dirs::home_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )
            .env(
                "ORQELLA_MODULE_CONFIG",
                serde_json::to_string(&module.state.config).unwrap_or_else(|_| "{}".into()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        #[cfg(target_os = "linux")]
        {
            // The service unit uses KillMode=process so QEMU guests survive a
            // daemon restart. Sidecars must not outlive the daemon and hold
            // stale loopback ports, so tie their lifetime to the daemon.
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        let job = if self.policy.max_memory_mb > 0 {
            Some(containment::create_job(self.policy.max_memory_mb)?)
        } else {
            None
        };
        let child = command
            .spawn()
            .with_context(|| format!("could not start sidecar for {}", module.manifest.id))?;
        #[cfg(windows)]
        if let Some(job) = &job
            && let Err(error) = containment::assign_job(job, &child)
        {
            let mut child = child;
            let _ = child.start_kill();
            bail!("could not contain sidecar: {error}");
        }

        {
            let mut processes = self.processes.lock().expect("sidecar lock");
            processes.insert(
                module.manifest.id.clone(),
                Sidecar {
                    child,
                    port,
                    token: token.clone(),
                    log_path: log_path.clone(),
                    #[cfg(windows)]
                    _job: job,
                },
            );
        }

        let health_url = format!(
            "http://127.0.0.1:{}{}",
            port,
            normalize_path(&module.manifest.backend.health_path)
        );
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(module.manifest.backend.ready_timeout_ms);
        loop {
            if self
                .client
                .get(&health_url)
                .header("X-Orqella-Module-Token", &token)
                .timeout(std::time::Duration::from_millis(700))
                .send()
                .await
                .map(|response| response.status().is_success())
                .unwrap_or(false)
            {
                self.clear_failure(&module.manifest.id);
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                self.remove_process(&module.manifest.id);
                bail!(
                    "sidecar for {} did not become ready at {}",
                    module.manifest.id,
                    health_url
                );
            }
            {
                let mut processes = self.processes.lock().expect("sidecar lock");
                if let Some(process) = processes.get_mut(&module.manifest.id)
                    && let Ok(Some(status)) = process.child.try_wait()
                {
                    processes.remove(&module.manifest.id);
                    bail!(
                        "sidecar for {} exited before becoming ready: {status}",
                        module.manifest.id
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.processes
            .lock()
            .expect("sidecar lock")
            .contains_key(id)
    }

    fn remove_process(&self, id: &str) -> bool {
        if let Some(process) = self.processes.lock().expect("sidecar lock").remove(id) {
            reap_child(process.child);
            true
        } else {
            false
        }
    }

    pub fn stop(&self, id: &str) {
        self.remove_process(id);
        self.clear_failure(id);
    }

    pub fn status(&self, id: &str) -> SidecarStatus {
        if self.is_running(id) {
            return SidecarStatus {
                state: "running".into(),
                attempts: 0,
                last_error: String::new(),
                next_retry_in_secs: 0,
            };
        }
        let failures = self.failures.lock().expect("sidecar failure lock");
        let Some(failure) = failures.get(id) else {
            return SidecarStatus {
                state: "stopped".into(),
                attempts: 0,
                last_error: String::new(),
                next_retry_in_secs: 0,
            };
        };
        SidecarStatus {
            state: if failure.attempts >= FAILED_AFTER_ATTEMPTS {
                "failed".into()
            } else {
                "retrying".into()
            },
            attempts: failure.attempts,
            last_error: failure.last_error.clone(),
            next_retry_in_secs: failure
                .next_attempt
                .saturating_duration_since(Instant::now())
                .as_secs(),
        }
    }

    pub fn should_attempt(&self, id: &str) -> bool {
        let failures = self.failures.lock().expect("sidecar failure lock");
        failures
            .get(id)
            .is_none_or(|failure| Instant::now() >= failure.next_attempt)
    }

    pub fn record_failure(&self, id: &str, error: impl Into<String>) -> SidecarStatus {
        let mut failures = self.failures.lock().expect("sidecar failure lock");
        let attempts = failures.get(id).map_or(1, |failure| failure.attempts + 1);
        let delay = backoff_seconds(attempts);
        failures.insert(
            id.to_string(),
            SidecarFailure {
                attempts,
                last_error: error.into(),
                next_attempt: Instant::now() + Duration::from_secs(delay),
            },
        );
        SidecarStatus {
            state: if attempts >= FAILED_AFTER_ATTEMPTS {
                "failed".into()
            } else {
                "retrying".into()
            },
            attempts,
            last_error: failures
                .get(id)
                .map(|failure| failure.last_error.clone())
                .unwrap_or_default(),
            next_retry_in_secs: delay,
        }
    }

    pub fn clear_failure(&self, id: &str) {
        self.failures
            .lock()
            .expect("sidecar failure lock")
            .remove(id);
    }

    pub fn reap(&self) -> Vec<String> {
        let mut processes = self.processes.lock().expect("sidecar lock");
        let mut dead = Vec::new();
        for (id, process) in processes.iter_mut() {
            if let Ok(Some(_)) = process.child.try_wait() {
                dead.push(id.clone());
            }
        }
        for id in &dead {
            processes.remove(id);
        }
        dead
    }

    pub fn trim_logs(&self, max_bytes: u64) {
        let paths = self
            .processes
            .lock()
            .expect("sidecar lock")
            .values()
            .map(|process| process.log_path.clone())
            .collect::<Vec<_>>();
        for path in paths {
            let too_large = std::fs::metadata(&path)
                .map(|metadata| metadata.len() > max_bytes)
                .unwrap_or(false);
            if too_large && let Ok(file) = crate::util::open_append_no_follow(&path) {
                let _ = file.set_len(0);
            }
        }
    }

    pub fn stop_all(&self) {
        let mut processes = self.processes.lock().expect("sidecar lock");
        for (_, process) in processes.drain() {
            reap_child(process.child);
        }
        self.failures.lock().expect("sidecar failure lock").clear();
    }

    pub fn websocket_target(&self, id: &str) -> Result<(u16, String)> {
        let processes = self.processes.lock().expect("sidecar lock");
        let process = processes.get(id).context("module sidecar is not running")?;
        Ok((process.port, process.token.clone()))
    }

    pub async fn request(
        &self,
        id: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let (port, token) = {
            let processes = self.processes.lock().expect("sidecar lock");
            let process = processes.get(id).context("module sidecar is not running")?;
            (process.port, process.token.clone())
        };
        let method = Method::from_bytes(method.as_bytes()).context("invalid HTTP method")?;
        let url = format!("http://127.0.0.1:{}{}", port, normalize_path(path));
        let mut request = self
            .client
            .request(method, url)
            .header("X-Orqella-Module-Token", token)
            .header("X-Orqella-Module-Id", id);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .context("sidecar request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let body: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "text": text }));
        if !status.is_success() {
            bail!("sidecar returned {status}: {body}");
        }
        Ok(body)
    }
}

fn backoff_seconds(attempts: u32) -> u64 {
    let exponent = attempts.saturating_sub(1).min(6);
    RETRY_BASE_SECS
        .saturating_mul(1u64 << exponent)
        .min(RETRY_MAX_SECS)
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn ensure_regular_file_beneath(path: &Path, root: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("could not resolve {}", root.display()))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve {}", path.display()))?;
    if !path.starts_with(&root) {
        bail!("{} escapes {}", path.display(), root.display());
    }
    Ok(())
}

fn set_artifact_executable(_path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(_path)
        .with_context(|| format!("could not read {}", _path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("{} is not a regular file", _path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(_path, permissions)
            .with_context(|| format!("could not make {} executable", _path.display()))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sandboxed_command(
    root: &Path,
    sandbox_dir: &Path,
    module_id: &str,
    sandbox: bool,
    network: bool,
    executable: &str,
    args: &[String],
    max_memory_mb: u64,
    read_paths: &[String],
    write_paths: &[String],
) -> Result<(String, Vec<String>)> {
    let program = executable.to_string();
    let program_args = args.to_vec();
    #[cfg(target_os = "linux")]
    {
        let limits = max_memory_mb > 0 && command_exists("prlimit");
        if sandbox && command_exists("bwrap") {
            let root = root.to_string_lossy().to_string();
            let mut final_program = program.clone();
            let mut final_args = program_args.clone();
            if limits {
                let mut limited = vec![
                    format!("--as={}", max_memory_mb.saturating_mul(1024 * 1024)),
                    "--".into(),
                    final_program.clone(),
                ];
                limited.append(&mut final_args);
                final_program = "prlimit".into();
                final_args = limited;
            }
            let mut wrapped = Vec::new();
            for path in ["/usr", "/bin", "/lib", "/lib64", "/etc"] {
                if Path::new(path).exists() {
                    wrapped.push("--ro-bind".into());
                    wrapped.push(path.into());
                    wrapped.push(path.into());
                }
            }
            wrapped.extend([
                "--unshare-pid".into(),
                "--unshare-ipc".into(),
                "--unshare-uts".into(),
                "--unshare-cgroup".into(),
                "--dev".into(),
                "/dev".into(),
                "--proc".into(),
                "/proc".into(),
                "--tmpfs".into(),
                "/tmp".into(),
                "--bind".into(),
                root.clone(),
                root.clone(),
            ]);
            // Git metadata and the manifest drive daemon-side behavior. A
            // sandboxed sidecar does not need to rewrite either, and allowing
            // it to do so turns the next unsandboxed `git status`/update into a
            // code-execution primitive.
            for protected in [".git", "module.toml"] {
                let path = Path::new(&root).join(protected);
                if path.exists() {
                    let path = path.to_string_lossy().to_string();
                    wrapped.push("--ro-bind".into());
                    wrapped.push(path.clone());
                    wrapped.push(path);
                }
            }
            // Read-only paths are mounted after the writable root so a module
            // cannot re-grant write access to a path it declared read-only.
            for path in read_paths {
                let expanded = expand_read_path(path);
                if std::fs::symlink_metadata(&expanded)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    continue;
                }
                let path = expanded.canonicalize().unwrap_or(expanded);
                if path.exists() {
                    let path = path.to_string_lossy().to_string();
                    wrapped.push("--ro-bind".into());
                    wrapped.push(path.clone());
                    wrapped.push(path);
                }
            }
            for path in write_paths {
                let path = Path::new(path);
                if path.exists() {
                    let path = path.to_string_lossy().to_string();
                    wrapped.push("--bind".into());
                    wrapped.push(path.clone());
                    wrapped.push(path);
                }
            }
            wrapped.extend([
                "--die-with-parent".into(),
                "--new-session".into(),
                "--chdir".into(),
                root,
                "--".into(),
                final_program,
            ]);
            wrapped.append(&mut final_args);
            return Ok(("bwrap".into(), wrapped));
        }
        if limits {
            let mut wrapped = vec![
                format!("--as={}", max_memory_mb.saturating_mul(1024 * 1024)),
                "--".into(),
                program.clone(),
            ];
            wrapped.append(&mut program_args.clone());
            return Ok(("prlimit".into(), wrapped));
        }
    }
    #[cfg(target_os = "macos")]
    if sandbox && command_exists("sandbox-exec") {
        std::fs::create_dir_all(sandbox_dir)?;
        let profile = sandbox_dir.join(format!("sidecar-{module_id}.sb"));
        std::fs::write(
            &profile,
            containment::macos_profile(root, read_paths, write_paths),
        )?;
        let mut wrapped = vec![
            "-f".into(),
            profile.to_string_lossy().to_string(),
            "--".into(),
            program,
        ];
        wrapped.extend(program_args);
        return Ok(("sandbox-exec".into(), wrapped));
    }
    let _ = (
        root,
        sandbox_dir,
        module_id,
        sandbox,
        network,
        max_memory_mb,
        read_paths,
        write_paths,
    );
    Ok((program, program_args))
}

fn expand_read_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn command_exists(name: &str) -> bool {
    command_in_path(name)
}

struct PythonSetup {
    module_id: String,
    root: PathBuf,
    venv_root: PathBuf,
    python: Option<String>,
    requirements: Option<String>,
    command: Vec<String>,
    sandbox: bool,
    sandbox_dir: PathBuf,
    max_memory_mb: u64,
    read_paths: Vec<String>,
}

struct PythonInterpreter {
    program: String,
    prefix: Vec<String>,
}

fn python_candidates() -> Vec<PythonInterpreter> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    {
        for version in ["3.13", "3.12", "3.11", "3.10"] {
            candidates.push(PythonInterpreter {
                program: "py".into(),
                prefix: vec![format!("-{version}")],
            });
        }
        candidates.push(PythonInterpreter {
            program: "py".into(),
            prefix: vec!["-3".into()],
        });
    }
    for name in [
        "python3.13",
        "python3.12",
        "python3.11",
        "python3.10",
        "python3",
        "python",
    ] {
        candidates.push(PythonInterpreter {
            program: name.into(),
            prefix: Vec::new(),
        });
    }
    candidates
}

fn python_version(interpreter: &PythonInterpreter) -> Option<(u32, u32)> {
    let output = std::process::Command::new(&interpreter.program)
        .args(&interpreter.prefix)
        .args([
            "-c",
            "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.trim().split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn python_satisfies(version: (u32, u32), spec: &str) -> bool {
    let spec = spec.trim();
    let (operator, raw) = if let Some(rest) = spec.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = spec.strip_prefix(">") {
        (">", rest)
    } else if let Some(rest) = spec.strip_prefix("<=") {
        ("<=", rest)
    } else if let Some(rest) = spec.strip_prefix("<") {
        ("<", rest)
    } else if let Some(rest) = spec.strip_prefix("==") {
        ("==", rest)
    } else {
        ("==", spec)
    };
    let mut parts = raw.trim().split('.');
    let Some(required) = parts
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .zip(parts.next().and_then(|minor| minor.parse::<u32>().ok()))
    else {
        return false;
    };
    match operator {
        ">=" => version >= required,
        ">" => version > required,
        "<=" => version <= required,
        "<" => version < required,
        "==" => version == required,
        _ => false,
    }
}

fn python_install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Install it with `brew install python@3.12` (or newer) and retry the module."
    } else if cfg!(windows) {
        "Install it with `winget install Python.Python.3.12` (or newer) and retry the module."
    } else {
        "Install Python 3 with your distribution's package manager and retry the module."
    }
}

fn find_python(setup: &PythonSetup) -> Result<PythonInterpreter> {
    let spec = setup.python.as_deref().unwrap_or("");
    for interpreter in python_candidates() {
        if !command_in_path(&interpreter.program) {
            continue;
        }
        let Some(version) = python_version(&interpreter) else {
            continue;
        };
        if spec.is_empty() || python_satisfies(version, spec) {
            return Ok(interpreter);
        }
    }
    let required = if spec.is_empty() { "Python" } else { spec };
    bail!(
        "module `{}` needs {required}, but no matching interpreter was found. {}",
        setup.module_id,
        python_install_hint()
    )
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn requirements_digest(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn output_tail(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}\n{stderr}");
    let text = text.trim();
    if text.chars().count() > 4000 {
        let tail = text.chars().rev().take(4000).collect::<String>();
        format!("…{}", tail.chars().rev().collect::<String>())
    } else {
        text.to_string()
    }
}

fn venv_version(venv: &Path) -> Option<(u32, u32)> {
    let raw = std::fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;
    let value = raw.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        matches!(key.trim(), "version" | "version_info").then(|| value.trim())
    })?;
    let mut parts = value.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn prepare_python(setup: &PythonSetup) -> Result<(String, Vec<String>)> {
    let interpreter = find_python(setup)?;
    let spec = setup.python.as_deref().unwrap_or("");
    let mut args;
    let program = if let Some(requirements) = &setup.requirements {
        let requirements = setup.root.join(requirements);
        let venv = &setup.venv_root;
        let venv_python = venv_python(venv);
        // Never execute the managed venv interpreter outside the sandbox.
        // A module can overwrite venv/bin/python during pip install; only
        // inspect the declarative pyvenv.cfg here and run the real sidecar
        // inside bubblewrap.
        let reuse = venv_python.is_file()
            && venv_version(venv)
                .is_some_and(|version| spec.is_empty() || python_satisfies(version, spec));
        if !reuse {
            if venv.exists() {
                std::fs::remove_dir_all(venv)
                    .with_context(|| format!("could not replace {}", venv.display()))?;
            }
            let output = std::process::Command::new(&interpreter.program)
                .args(&interpreter.prefix)
                .args(["-m", "venv"])
                .arg(venv)
                .env_clear()
                .env(
                    "PATH",
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
                )
                .env("HOME", venv)
                .env("TMPDIR", "/tmp")
                .output()
                .with_context(|| format!("could not create {}", venv.display()))?;
            if !output.status.success() {
                bail!(
                    "could not create the module Python environment: {}",
                    output_tail(&output)
                );
            }
        }
        let digest = requirements_digest(&requirements)?;
        let stamp = venv.join(".requirements.sha256");
        let changed = std::fs::read_to_string(&stamp)
            .map(|value| value.trim() != digest)
            .unwrap_or(true);
        if changed {
            let pip_args = vec![
                "-m".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "--disable-pip-version-check".to_string(),
                "-r".to_string(),
                requirements.to_string_lossy().to_string(),
            ];
            let venv_arg = venv.to_string_lossy().to_string();
            let (program, args) = if setup.sandbox {
                sandboxed_command(
                    &setup.root,
                    &setup.sandbox_dir,
                    &setup.module_id,
                    true,
                    true, // dependency installation needs network
                    &venv_python.to_string_lossy(),
                    &pip_args,
                    setup.max_memory_mb,
                    &setup.read_paths,
                    &[venv_arg],
                )?
            } else {
                (venv_python.to_string_lossy().to_string(), pip_args)
            };
            let output = std::process::Command::new(program)
                .args(args)
                .env_clear()
                .env(
                    "PATH",
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
                )
                .env("HOME", venv)
                .env("TMPDIR", "/tmp")
                .output()
                .with_context(|| format!("could not run pip for {}", setup.module_id))?;
            if !output.status.success() {
                bail!(
                    "module `{}` dependencies failed to install: {}",
                    setup.module_id,
                    output_tail(&output)
                );
            }
            std::fs::write(&stamp, format!("{digest}\n"))?;
        }
        args = Vec::new();
        venv_python.to_string_lossy().to_string()
    } else {
        args = interpreter.prefix.clone();
        interpreter.program.clone()
    };
    args.extend(setup.command.iter().skip(1).cloned());
    Ok((program, args))
}

fn resolve_command(root: &Path, command: &str) -> String {
    let candidate = root.join(command);
    if command.contains(std::path::MAIN_SEPARATOR) && candidate.is_file() {
        return candidate.to_string_lossy().to_string();
    }
    command.to_string()
}

/// Resolve a sidecar command and its arguments. Most commands are used
/// verbatim; `python3` gets a small cross-platform fallback because Windows
/// installs commonly expose only `python` or the `py` launcher.
fn resolve_sidecar_command(root: &Path, args: &[String]) -> (String, Vec<String>) {
    let rest = args[1..].to_vec();
    let program = &args[0];
    if program == "python3" && !command_in_path(program) {
        if command_in_path("python") {
            return ("python".into(), rest);
        }
        #[cfg(windows)]
        if command_in_path("py") {
            let mut py_args = vec!["-3".to_string()];
            py_args.extend(rest);
            return ("py".into(), py_args);
        }
    }
    (resolve_command(root, program), rest)
}

fn command_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    #[cfg(windows)]
    let names = {
        let mut values = vec![name.to_string()];
        if let Ok(exts) = std::env::var("PATHEXT") {
            values.extend(
                exts.split(';')
                    .filter(|ext| !ext.is_empty())
                    .map(|ext| format!("{name}{ext}")),
            );
        }
        values
    };
    #[cfg(not(windows))]
    let names = [name.to_string()];
    std::env::split_paths(&path).any(|dir| {
        names.iter().any(|candidate| {
            let path = dir.join(candidate);
            path.is_file()
        })
    })
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn reap_child(mut child: Child) {
    let _ = child.start_kill();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = child.wait().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(backoff_seconds(1), 8);
        assert_eq!(backoff_seconds(2), 16);
        assert_eq!(backoff_seconds(5), 128);
        assert_eq!(backoff_seconds(7), 300);
        assert_eq!(backoff_seconds(50), 300);
    }

    #[test]
    fn failures_enter_a_visible_retry_state() {
        let manager = SidecarManager::default();
        assert_eq!(manager.status("demo").state, "stopped");
        assert!(manager.should_attempt("demo"));
        for attempt in 1..=5 {
            let status = manager.record_failure("demo", format!("boom {attempt}"));
            assert_eq!(status.attempts, attempt);
            assert_eq!(
                status.state,
                if attempt >= 5 { "failed" } else { "retrying" }
            );
            assert_eq!(status.last_error, format!("boom {attempt}"));
            assert!(!manager.should_attempt("demo"));
        }
        manager.clear_failure("demo");
        assert!(manager.should_attempt("demo"));
        assert_eq!(manager.status("demo").state, "stopped");
    }

    #[test]
    fn hashes_sidecar_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("artifact");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn downloads_and_verifies_a_sidecar_artifact() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = b"sidecar executable".to_vec();
        let expected = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut socket, &body)
                .await
                .unwrap();
        });

        let manager = SidecarManager::default();
        let artifact = BackendArtifact {
            platform: host_platform(),
            url: format!("http://{address}/sidecar"),
            path: String::new(),
            sha256: hex::encode(Sha256::digest(&expected)),
            args: Vec::new(),
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sidecar");
        manager.download_artifact(&artifact, &path).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn selects_a_module_local_sidecar_artifact() {
        use crate::modules::manifest::Manifest;
        use crate::modules::store::ModuleState;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::create_dir(root.path().join("backend")).unwrap();
        std::fs::create_dir(root.path().join("backend/bin")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        let body = b"#!/bin/sh\nexit 0\n";
        std::fs::write(root.path().join("backend/bin/sidecar"), body).unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            format!(
                r#"schema = 1
id = "com.example.local-artifact"
name = "Local artifact"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "{}"
path = "backend/bin/sidecar"
sha256 = "{}"
args = ["--serve"]
"#,
                host_platform(),
                hex::encode(Sha256::digest(body))
            ),
        )
        .unwrap();
        let module = InstalledModule {
            manifest: Manifest::load(root.path()).unwrap(),
            root: root.path().to_path_buf(),
            state: ModuleState::default(),
        };
        let manager = SidecarManager::default();
        let (program, args) = manager.sidecar_command(&module).await.unwrap();
        assert_eq!(
            program,
            root.path().join("backend/bin/sidecar").to_string_lossy()
        );
        assert_eq!(args, vec!["--serve"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&program).unwrap().permissions().mode() & 0o111,
                0
            );
        }
    }

    #[tokio::test]
    async fn selects_downloads_and_reuses_a_sidecar_artifact() {
        use crate::modules::manifest::Manifest;
        use crate::modules::store::ModuleState;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = b"#!/bin/sh\nexit 0\n".to_vec();
        let expected = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut socket, &body)
                .await
                .unwrap();
        });

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            format!(
                r#"schema = 1
id = "com.example.artifact"
name = "Artifact"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "{}"
url = "http://{address}/sidecar"
sha256 = "{}"
args = ["--serve"]
"#,
                host_platform(),
                hex::encode(Sha256::digest(&expected))
            ),
        )
        .unwrap();
        let module = InstalledModule {
            manifest: Manifest::load(root.path()).unwrap(),
            root: root.path().to_path_buf(),
            state: ModuleState::default(),
        };
        let manager = SidecarManager::default();
        let (program, args) = manager.sidecar_command(&module).await.unwrap();
        let artifact_path = root.path().join("data/.sidecar-artifacts");
        assert!(Path::new(&program).starts_with(&artifact_path));
        assert_eq!(args, vec!["--serve"]);
        assert_eq!(std::fs::read(&program).unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&program).unwrap().permissions().mode() & 0o111,
                0
            );
        }

        server.await.unwrap();
        let (cached, cached_args) = manager.sidecar_command(&module).await.unwrap();
        assert_eq!(cached, program);
        assert_eq!(cached_args, args);
    }

    #[test]
    fn python_requirement_specs_handle_ranges_and_garbage() {
        assert!(python_satisfies((3, 12), ">3.11"));
        assert!(!python_satisfies((3, 11), ">3.11"));
        assert!(python_satisfies((3, 10), "<=3.10"));
        assert!(python_satisfies((3, 10), "<3.11"));
        assert!(python_satisfies((3, 11), "==3.11"));
        assert!(python_satisfies((3, 11), "3.11"));
        for spec in ["", ">=", ">=3", ">=3.x", "!=3.11", ">>3.10"] {
            assert!(!python_satisfies((3, 12), spec), "{spec}");
        }
    }

    #[test]
    fn sidecar_commands_resolve_module_local_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("backend")).unwrap();
        std::fs::write(root.path().join("backend/run"), "#!/bin/sh\n").unwrap();
        let (program, args) = resolve_sidecar_command(
            root.path(),
            &["backend/run".to_string(), "--serve".to_string()],
        );
        assert_eq!(program, root.path().join("backend/run").to_string_lossy());
        assert_eq!(args, vec!["--serve"]);

        let (program, args) = resolve_sidecar_command(
            root.path(),
            &["python3".to_string(), "server.py".to_string()],
        );
        assert!(!program.is_empty());
        assert_eq!(args, vec!["server.py"]);
    }

    #[test]
    fn read_paths_expand_only_the_home_prefix() {
        let home = dirs::home_dir().expect("home directory");
        assert_eq!(expand_read_path("~/models"), home.join("models"));
        assert_eq!(
            expand_read_path("/opt/models"),
            PathBuf::from("/opt/models")
        );
        assert_eq!(expand_read_path("relative"), PathBuf::from("relative"));
    }

    #[test]
    fn protected_read_paths_reject_tilde_prefixed_and_nested_paths() {
        let home = dirs::home_dir().expect("home directory");
        let manager = SidecarManager::new(
            SidecarPolicy::default(),
            PathBuf::from("/tmp/run"),
            PathBuf::from("/tmp/logs"),
            PathBuf::from("/tmp/venvs"),
            vec![home.join(".local/share/orqella")],
        );
        assert!(manager.protected_read_path("~/.local/share/orqella"));
        assert!(manager.protected_read_path("~/.local/share/orqella/admin.token"));
        assert!(!manager.protected_read_path("/opt/models"));
    }

    #[test]
    fn python_requirements_are_compared_by_major_minor() {
        assert!(python_satisfies((3, 12), ">=3.11"));
        assert!(python_satisfies((3, 11), ">=3.11"));
        assert!(!python_satisfies((3, 10), ">=3.11"));
        assert!(python_satisfies((3, 10), ">=3.10"));
        assert!(python_satisfies((3, 10), "==3.10"));
        assert!(!python_satisfies((3, 8), ">3.8"));
        assert!(!python_satisfies((3, 9), "invalid"));
    }

    #[test]
    fn missing_python_requirement_is_actionable() {
        let setup = PythonSetup {
            module_id: "test-module".into(),
            root: std::env::temp_dir(),
            venv_root: std::env::temp_dir().join("orqella-test-venv"),
            python: Some(">=99.0".into()),
            requirements: None,
            command: vec!["python3".into(), "backend/server.py".into()],
            sandbox: false,
            sandbox_dir: std::env::temp_dir().join("orqella-test-sandbox"),
            max_memory_mb: 0,
            read_paths: Vec::new(),
        };
        let error = prepare_python(&setup).unwrap_err().to_string();
        assert!(error.contains("needs >=99.0"));
        assert!(error.contains("test-module"));
    }

    #[test]
    fn legacy_sidecar_home_moves_under_data_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".config/pulse")).unwrap();
        std::fs::write(root.join(".config/pulse/cookie"), "cookie").unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let home = prepare_module_home(root).unwrap();
        assert_eq!(home, root.join("data/home"));
        assert!(home.join(".config/pulse/cookie").is_file());
        assert!(home.join(".orqella-home-v1").is_file());
        assert!(!root.join(".config").exists());
        assert!(!root.join(".cache").exists());
        assert!(root.join(".git").is_dir());

        std::fs::create_dir_all(root.join(".npm")).unwrap();
        assert_eq!(prepare_module_home(root).unwrap(), home);
        assert!(root.join(".npm").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_home_cannot_be_a_symlink() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("module");
        let other = temp.path().join("elsewhere");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        symlink(&other, root.join("data/home")).unwrap();
        assert!(prepare_module_home(&root).is_err());
    }
}
