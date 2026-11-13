use crate::config::Paths;
use crate::modules::git;
use crate::modules::manifest::{ConfigFieldType, Manifest, ManifestSummary};
use crate::util;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleState {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub granted_permissions: Vec<String>,
    #[serde(default)]
    pub pending_permissions: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub installed_at: String,
}

fn default_true() -> bool {
    true
}

impl Default for ModuleState {
    fn default() -> Self {
        Self {
            enabled: true,
            granted_permissions: Vec::new(),
            pending_permissions: Vec::new(),
            config: BTreeMap::new(),
            source: "local".into(),
            installed_at: util::now_iso(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub modules: HashMap<String, ModuleState>,
}

fn state_version() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct InstalledModule {
    pub manifest: Manifest,
    pub root: PathBuf,
    pub state: ModuleState,
}

impl InstalledModule {
    pub fn grants(&self, permission: &str) -> bool {
        self.state
            .granted_permissions
            .iter()
            .any(|granted| granted == permission)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleView {
    #[serde(flatten)]
    pub manifest: ManifestSummary,
    pub enabled: bool,
    pub granted_permissions: Vec<String>,
    pub pending_permissions: Vec<String>,
    pub config_schema: crate::modules::manifest::ModuleConfigSchema,
    pub config: BTreeMap<String, serde_json::Value>,
    pub source: String,
    pub installed_at: String,
    pub git: ModuleGitView,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleGitView {
    pub is_repo: bool,
    pub repository: Option<String>,
    pub repository_name: Option<String>,
    /// GitHub repository recorded when the module was installed. Unlike
    /// `repository_name`, this is not derived from the module-writable
    /// `.git/config`.
    pub source_repository: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub commit_short: Option<String>,
    pub dirty: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

impl From<git::RepoStatus> for ModuleGitView {
    fn from(status: git::RepoStatus) -> Self {
        Self {
            is_repo: status.is_repo,
            repository_name: status
                .repository
                .as_deref()
                .and_then(git::github_repository),
            source_repository: None,
            repository: status.repository,
            branch: status.branch,
            commit_short: status.commit.as_deref().map(git::short_commit),
            commit: status.commit,
            dirty: status.dirty,
            upstream: status.upstream,
            ahead: status.ahead,
            behind: status.behind,
        }
    }
}

impl ModuleView {
    fn from_module(module: &InstalledModule, status: git::RepoStatus) -> Self {
        let mut git: ModuleGitView = status.into();
        git.source_repository = module
            .state
            .source
            .strip_prefix("git:")
            .and_then(|value| value.split('#').next())
            .and_then(git::github_repository);
        Self {
            manifest: module.manifest.summary(),
            enabled: module.state.enabled,
            granted_permissions: module.state.granted_permissions.clone(),
            pending_permissions: module.state.pending_permissions.clone(),
            config_schema: module.manifest.config.clone(),
            config: module.state.config.clone(),
            source: module.state.source.clone(),
            installed_at: module.state.installed_at.clone(),
            git,
        }
    }
}

struct CachedGitStatus {
    at: Instant,
    status: git::RepoStatus,
}

pub struct ModuleStore {
    paths: Paths,
    modules: RwLock<BTreeMap<String, InstalledModule>>,
    state: RwLock<PersistedState>,
    /// Git status is relatively expensive (several subprocesses per module).
    /// List views may reuse a very short-lived snapshot; mutations invalidate
    /// it explicitly.
    git_cache: Mutex<HashMap<String, CachedGitStatus>>,
    /// Serializes install/update/remove/permission mutations. Reads remain
    /// lock-free beyond the module map lock, but mutations must not race on a
    /// checkout or on the shared backup directory.
    operation_lock: Mutex<()>,
}

impl ModuleStore {
    pub async fn load(paths: Paths) -> Result<Self> {
        let state = match load_state(&paths.state_file) {
            Ok(state) => state,
            Err(error) => {
                let backup = paths
                    .state_file
                    .with_extension(format!("corrupt.{}", chrono::Utc::now().timestamp()));
                if paths.state_file.is_file() {
                    let _ = std::fs::copy(&paths.state_file, &backup);
                    let _ = crate::util::restrict_file(&backup);
                }
                tracing::error!(
                    %error,
                    backup = %backup.display(),
                    "module state is invalid; starting with conservative permissions"
                );
                PersistedState::default()
            }
        };
        if paths.state_file.is_file() {
            let _ = crate::util::restrict_file(&paths.state_file);
        }
        let store = Self {
            paths,
            modules: RwLock::new(BTreeMap::new()),
            state: RwLock::new(state),
            git_cache: Mutex::new(HashMap::new()),
            operation_lock: Mutex::new(()),
        };
        store.scan()?;
        store.migrate_sources()?;
        Ok(store)
    }

    fn scan(&self) -> Result<()> {
        let mut modules = BTreeMap::new();
        if !self.paths.modules_dir.exists() {
            std::fs::create_dir_all(&self.paths.modules_dir)?;
        }
        for entry in std::fs::read_dir(&self.paths.modules_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let root = entry.path();
            let manifest = match Manifest::load(&root) {
                Ok(manifest) => manifest,
                Err(error) => {
                    tracing::warn!(path = %root.display(), %error, "skipping invalid module");
                    continue;
                }
            };
            match git::ensure_repo(
                &root,
                &format!(
                    "Import {} {} into its module repository",
                    manifest.name, manifest.version
                ),
            ) {
                Ok(true) => tracing::info!(module = %manifest.id, "initialized module repository"),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(module = %manifest.id, %error, "module is not a git checkout")
                }
            }
            let mut state = self
                .state
                .read()
                .expect("module state lock")
                .modules
                .get(&manifest.id)
                .cloned()
                .unwrap_or_else(|| conservative_module_state(&manifest));
            state.config = merge_config(state.config.clone(), &manifest);
            state
                .granted_permissions
                .retain(|permission| manifest.permissions.contains(permission));
            state.pending_permissions = manifest
                .permissions
                .iter()
                .filter(|permission| !state.granted_permissions.contains(permission))
                .cloned()
                .collect();
            modules.insert(
                manifest.id.clone(),
                InstalledModule {
                    manifest,
                    root,
                    state,
                },
            );
        }
        *self.modules.write().expect("module lock") = modules;
        self.git_cache.lock().expect("git cache lock").clear();
        Ok(())
    }

    fn migrate_sources(&self) -> Result<()> {
        let mut changed = false;
        {
            let mut modules = self.modules.write().expect("module lock");
            let mut persisted = self.state.write().expect("state lock");
            for module in modules.values_mut() {
                let source = if module.state.source == "builtin" {
                    Some(format!("local:{}", module.root.display()))
                } else if let Some(path) = module.state.source.strip_prefix("local:") {
                    if Path::new(path).exists() {
                        None
                    } else {
                        // The original authoring folder moved or was removed.
                        Some(format!("local:{}", module.root.display()))
                    }
                } else {
                    None
                };
                let Some(source) = source else {
                    continue;
                };
                module.state.source = source.clone();
                if let Some(record) = persisted.modules.get_mut(&module.manifest.id) {
                    record.source = source;
                }
                changed = true;
            }
        }
        if changed {
            self.save_state()?;
        }
        Ok(())
    }

    fn save_state(&self) -> Result<()> {
        let state = self.state.read().expect("state lock").clone();
        let raw = serde_json::to_vec_pretty(&state)?;
        util::write_atomic(&self.paths.state_file, &raw)?;
        Ok(())
    }

    fn git_status(&self, module: &InstalledModule) -> git::RepoStatus {
        const TTL: Duration = Duration::from_secs(2);
        let now = Instant::now();
        if let Some(cached) = self
            .git_cache
            .lock()
            .expect("git cache lock")
            .get(&module.manifest.id)
            .filter(|cached| now.duration_since(cached.at) < TTL)
        {
            return cached.status.clone();
        }
        let status = git::status(&module.root);
        self.git_cache.lock().expect("git cache lock").insert(
            module.manifest.id.clone(),
            CachedGitStatus {
                at: now,
                status: status.clone(),
            },
        );
        status
    }

    fn invalidate_git(&self, id: &str) {
        self.git_cache.lock().expect("git cache lock").remove(id);
    }

    fn view(&self, module: &InstalledModule) -> ModuleView {
        ModuleView::from_module(module, self.git_status(module))
    }

    pub fn list(&self) -> Vec<ModuleView> {
        // Do not hold the module map lock while running git status.
        let modules = self
            .modules
            .read()
            .expect("module lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        modules.iter().map(|module| self.view(module)).collect()
    }

    pub fn summaries(&self) -> Vec<ManifestSummary> {
        self.modules
            .read()
            .expect("module lock")
            .values()
            .map(|module| module.manifest.summary())
            .collect()
    }

    /// Cheap module snapshots for lifecycle work. Unlike `list`, this does not
    /// run git commands for every module.
    pub fn installed(&self) -> Vec<InstalledModule> {
        self.modules
            .read()
            .expect("module lock")
            .values()
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<InstalledModule> {
        self.modules.read().expect("module lock").get(id).cloned()
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        {
            let mut modules = self.modules.write().expect("module lock");
            let module = modules.get_mut(id).context("module not found")?;
            module.state.enabled = enabled;
        }
        {
            let mut state = self.state.write().expect("state lock");
            if let Some(record) = state.modules.get_mut(id) {
                record.enabled = enabled;
            }
        }
        self.save_state()
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        if self.get(id).is_none() {
            bail!("module not found");
        }
        let root = self.paths.module_dir(id);
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        self.modules.write().expect("module lock").remove(id);
        self.state.write().expect("state lock").modules.remove(id);
        self.invalidate_git(id);
        self.save_state()
    }

    pub fn install_from_path(&self, source: &Path) -> Result<ModuleView> {
        self.install_from_path_inner(
            source,
            true,
            false,
            Some(format!("local:{}", source.display())),
        )
    }

    pub fn install_from_path_restricted(&self, source: &Path) -> Result<ModuleView> {
        self.install_from_path_inner(
            source,
            false,
            false,
            Some(format!("local:{}", source.display())),
        )
    }

    /// Install an edited workspace while preserving the installed checkout's
    /// git history and origin. Used by the AI edit flow so a later update can
    /// still fast-forward from the published module repository.
    pub fn install_from_path_preserving_git(&self, source: &Path) -> Result<ModuleView> {
        self.install_from_path_inner(source, true, true, None)
    }

    pub fn install_from_source(&self, source: &str, branch: Option<&str>) -> Result<ModuleView> {
        let source = source.trim();
        if !git::is_remote(source) {
            bail!("module source must be a git URL");
        }
        let temp = tempfile::tempdir().context("could not create a module checkout")?;
        let checkout = temp.path().join("checkout");
        git::clone(source, branch, &checkout)?;
        let label = match branch {
            Some(branch) => format!("git:{source}#{branch}"),
            None => format!("git:{source}"),
        };
        self.install_from_path_inner(&checkout, true, true, Some(label))
    }

    fn install_from_path_inner(
        &self,
        source: &Path,
        auto_grant: bool,
        include_git: bool,
        source_label: Option<String>,
    ) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let root = find_module_root(source)?;
        let manifest = Manifest::load(&root)?;
        manifest.validate(&root)?;
        let destination = self.paths.module_dir(&manifest.id);
        let previous = self.get(&manifest.id);
        let backup = self.paths.data_dir.join("backups/modules").join(format!(
            "{}.{}",
            manifest.id,
            chrono::Utc::now().timestamp()
        ));
        if destination.exists() {
            if let Some(parent) = backup.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if backup.exists() {
                std::fs::remove_dir_all(&backup)?;
            }
            std::fs::rename(&destination, &backup)?;
        }
        if let Err(error) = copy_module_tree(&root, &destination, include_git) {
            restore_backup(&destination, &backup);
            return Err(error);
        }
        let installed = match Manifest::load(&destination).and_then(|manifest| {
            manifest.validate(&destination)?;
            Ok(manifest)
        }) {
            Ok(manifest) => manifest,
            Err(error) => {
                restore_backup(&destination, &backup);
                return Err(error);
            }
        };
        if let Err(error) = git::ensure_repo(
            &destination,
            &format!("Install {} {}", installed.name, installed.version),
        ) {
            restore_backup(&destination, &backup);
            return Err(error).context("could not store the module as a git repository");
        }
        let backup_data = backup.join("data");
        match std::fs::symlink_metadata(&backup_data) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                restore_backup(&destination, &backup);
                bail!("previous module data directory is a symlink; refusing to preserve it");
            }
            Ok(metadata) if metadata.is_dir() => {
                let data = destination.join("data");
                if data.exists() {
                    std::fs::remove_dir_all(&data)?;
                }
                std::fs::rename(&backup_data, &data)
                    .context("could not preserve module data during install")?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let source_changed = match (&source_label, &previous) {
            (Some(next), Some(previous)) => {
                !previous.state.source.is_empty() && previous.state.source != *next
            }
            _ => false,
        };
        let (mut granted_permissions, mut pending_permissions) = if let Some(previous) = &previous {
            if source_changed {
                // A different source replacing an existing module is a new
                // trust decision. Never carry a previous backend/agent grant
                // across that boundary without a fresh review.
                (Vec::new(), installed.permissions.clone())
            } else {
                let mut granted = previous.state.granted_permissions.clone();
                granted.retain(|permission| installed.permissions.contains(permission));
                let pending = installed
                    .permissions
                    .iter()
                    .filter(|permission| !granted.contains(permission))
                    .cloned()
                    .collect();
                (granted, pending)
            }
        } else if auto_grant {
            let granted: Vec<String> = installed
                .permissions
                .iter()
                .filter(|permission| !is_sensitive_permission(permission))
                .cloned()
                .collect();
            let pending = installed
                .permissions
                .iter()
                .filter(|permission| !granted.contains(permission))
                .cloned()
                .collect();
            (granted, pending)
        } else {
            (Vec::new(), installed.permissions.clone())
        };
        if let Some(previous) = &previous
            && backend_policy_changed(&previous.manifest, &installed)
        {
            // Sandbox policy is a security control, not ordinary metadata.
            // A source update that relaxes it must go back through the
            // permission review instead of silently running with more access.
            granted_permissions.retain(|permission| permission != "backend");
            if !pending_permissions
                .iter()
                .any(|permission| permission == "backend")
            {
                pending_permissions.push("backend".into());
            }
        }
        let previous_config = previous
            .as_ref()
            .map(|module| module.state.config.clone())
            .unwrap_or_default();
        let state = ModuleState {
            enabled: previous
                .as_ref()
                .map(|module| module.state.enabled)
                .unwrap_or(true),
            granted_permissions,
            pending_permissions,
            config: merge_config(previous_config, &installed),
            source: source_label
                .or_else(|| previous.as_ref().map(|module| module.state.source.clone()))
                .unwrap_or_else(|| format!("local:{}", source.display())),
            installed_at: util::now_iso(),
        };
        {
            let mut modules = self.modules.write().expect("module lock");
            modules.insert(
                installed.id.clone(),
                InstalledModule {
                    manifest: installed.clone(),
                    root: destination,
                    state: state.clone(),
                },
            );
        }
        {
            let mut persisted = self.state.write().expect("state lock");
            persisted
                .modules
                .insert(installed.id.clone(), state.clone());
        }
        self.save_state()?;
        self.invalidate_git(&installed.id);
        self.get(&installed.id)
            .map(|module| self.view(&module))
            .context("module disappeared after install")
    }

    /// Reload manifests and git metadata after files changed on disk.
    pub fn rescan(&self) -> Result<()> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        self.scan()
    }

    /// Commit the current module tree to its own repository.
    pub fn checkpoint(&self, id: &str, message: &str) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let module = self.get(id).context("module not found")?;
        git::ensure_repo(&module.root, message)?;
        git::commit_all(&module.root, message)?;
        self.invalidate_git(id);
        Ok(self.view(&module))
    }

    /// Fast-forward the module checkout from its configured upstream.
    pub fn update_from_git(&self, id: &str) -> Result<ModuleView> {
        self.update_from_git_with_token(id, None)
    }

    pub fn update_from_git_with_token(&self, id: &str, token: Option<&str>) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let module = self.get(id).context("module not found")?;
        let previous_manifest = module.manifest.clone();
        let previous = git::status(&module.root).commit;
        git::pull_with_token(&module.root, token)?;
        let reloaded = self.scan().and_then(|_| {
            let module = self.get(id).context("module disappeared after update")?;
            if backend_policy_changed(&previous_manifest, &module.manifest) {
                self.revoke_permission(id, "backend")?;
            }
            Ok(self.view(&self.get(id).context("module disappeared after update")?))
        });
        match reloaded {
            Ok(module) => Ok(module),
            Err(error) => {
                if let Some(previous) = previous {
                    let _ = git::run(&module.root, &["reset", "--hard", &previous]);
                    let _ = self.scan();
                }
                Err(error).context("update was rolled back to the previous commit")
            }
        }
    }

    fn revoke_permission(&self, id: &str, permission: &str) -> Result<()> {
        {
            let mut modules = self.modules.write().expect("module lock");
            let Some(module) = modules.get_mut(id) else {
                return Ok(());
            };
            module
                .state
                .granted_permissions
                .retain(|granted| granted != permission);
            if !module
                .state
                .pending_permissions
                .iter()
                .any(|pending| pending == permission)
            {
                module
                    .state
                    .pending_permissions
                    .push(permission.to_string());
            }
        }
        {
            let mut state = self.state.write().expect("state lock");
            if let Some(record) = state.modules.get_mut(id) {
                record
                    .granted_permissions
                    .retain(|granted| granted != permission);
                if !record
                    .pending_permissions
                    .iter()
                    .any(|pending| pending == permission)
                {
                    record.pending_permissions.push(permission.to_string());
                }
            }
        }
        self.save_state()
    }

    /// Remember the GitHub repository a module was published to.
    pub fn set_repository(&self, id: &str, url: &str, branch: &str) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let module = self.get(id).context("module not found")?;
        git::set_origin(&module.root, url)?;
        let source = format!("git:{url}#{branch}");
        {
            let mut modules = self.modules.write().expect("module lock");
            if let Some(module) = modules.get_mut(id) {
                module.state.source = source.clone();
            }
        }
        {
            let mut state = self.state.write().expect("state lock");
            if let Some(record) = state.modules.get_mut(id) {
                record.source = source;
            }
        }
        self.save_state()?;
        self.invalidate_git(id);
        self.get(id)
            .map(|module| self.view(&module))
            .context("module disappeared after repository update")
    }

    pub fn set_permissions(&self, id: &str, permissions: Vec<String>) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let module = self.get(id).context("module not found")?;
        for permission in &permissions {
            if !module.manifest.permissions.contains(permission) {
                bail!("module does not declare permission {permission}");
            }
        }
        {
            let mut modules = self.modules.write().expect("module lock");
            let module = modules.get_mut(id).context("module not found")?;
            module.state.granted_permissions = permissions.clone();
            module.state.pending_permissions = module
                .manifest
                .permissions
                .iter()
                .filter(|permission| !permissions.contains(permission))
                .cloned()
                .collect();
        }
        let pending = self
            .get(id)
            .map(|module| module.state.pending_permissions)
            .unwrap_or_default();
        {
            let mut state = self.state.write().expect("state lock");
            if let Some(record) = state.modules.get_mut(id) {
                record.granted_permissions = permissions;
                record.pending_permissions = pending;
            }
        }
        self.save_state()?;
        let module = self.get(id).context("module not found")?;
        Ok(self.view(&module))
    }
}

fn backend_policy_changed(previous: &Manifest, next: &Manifest) -> bool {
    let previous_sandbox = previous.backend.sandbox.unwrap_or(true);
    let next_sandbox = next.backend.sandbox.unwrap_or(true);
    let mut previous_paths = previous.backend.read_paths.clone();
    let mut next_paths = next.backend.read_paths.clone();
    previous_paths.sort();
    next_paths.sort();
    previous_sandbox != next_sandbox || previous_paths != next_paths
}

fn restore_backup(destination: &Path, backup: &Path) {
    if destination.exists() {
        let _ = std::fs::remove_dir_all(destination);
    }
    if backup.exists() {
        let _ = std::fs::rename(backup, destination);
    }
}

fn is_sensitive_permission(permission: &str) -> bool {
    matches!(
        permission,
        "agent" | "backend" | "media.record" | "app.open"
    ) || permission.starts_with("fs.read:")
        || permission.starts_with("fs.write:")
        || permission.starts_with("net.fetch:")
        || permission.starts_with("secrets:")
        || permission.starts_with("node.exec:")
        || permission.starts_with("node.proxy:")
        || permission.starts_with("node.action:")
}

/// State for a module that has no persisted record. Never grant a capability
/// that can execute code, access the microphone, or open an application
/// without an explicit review.
fn conservative_module_state(manifest: &Manifest) -> ModuleState {
    let (granted, pending) = manifest
        .permissions
        .iter()
        .cloned()
        .partition(|permission| !is_sensitive_permission(permission));
    ModuleState {
        granted_permissions: granted,
        pending_permissions: pending,
        ..ModuleState::default()
    }
}

impl ModuleStore {
    pub fn set_config(
        &self,
        id: &str,
        values: BTreeMap<String, serde_json::Value>,
    ) -> Result<ModuleView> {
        let _operation = self.operation_lock.lock().expect("module operation lock");
        let module = self.get(id).context("module not found")?;
        let mut next = module.state.config.clone();
        for field in &module.manifest.config.fields {
            let Some(value) = values.get(&field.key) else {
                bail!("missing config value {}", field.key);
            };
            validate_config_value(field, value)?;
            next.insert(field.key.clone(), value.clone());
        }
        if values.keys().any(|key| {
            !module
                .manifest
                .config
                .fields
                .iter()
                .any(|field| &field.key == key)
        }) {
            bail!("unknown config field");
        }
        {
            let mut modules = self.modules.write().expect("module lock");
            let module = modules.get_mut(id).context("module not found")?;
            module.state.config = next.clone();
        }
        {
            let mut state = self.state.write().expect("state lock");
            if let Some(record) = state.modules.get_mut(id) {
                record.config = next.clone();
            }
        }
        self.save_state()?;
        let module = self.get(id).context("module not found")?;
        Ok(self.view(&module))
    }
}

fn merge_config(
    existing: BTreeMap<String, serde_json::Value>,
    manifest: &Manifest,
) -> BTreeMap<String, serde_json::Value> {
    let mut values = existing;
    values.retain(|key, _| manifest.config.fields.iter().any(|field| &field.key == key));
    for field in &manifest.config.fields {
        if !values.contains_key(&field.key)
            && let Ok(value) = serde_json::to_value(&field.default)
        {
            values.insert(field.key.clone(), value);
        }
    }
    values
}

fn validate_config_value(
    field: &crate::modules::manifest::ConfigField,
    value: &serde_json::Value,
) -> Result<()> {
    match field.kind {
        ConfigFieldType::Boolean => {
            if !value.is_boolean() {
                bail!("config {} must be a boolean", field.key);
            }
        }
        ConfigFieldType::Number => {
            let Some(number) = value.as_f64() else {
                bail!("config {} must be a number", field.key);
            };
            if let Some(min) = field.min
                && number < min
            {
                bail!("config {} must be at least {min}", field.key);
            }
            if let Some(max) = field.max
                && number > max
            {
                bail!("config {} must be at most {max}", field.key);
            }
        }
        ConfigFieldType::Text => {
            if !value.is_string() {
                bail!("config {} must be text", field.key);
            }
        }
        ConfigFieldType::Select => {
            let Some(text) = value.as_str() else {
                bail!("config {} must be a selection", field.key);
            };
            if !field.options.iter().any(|option| option.value == text) {
                bail!("config {} has an unknown option", field.key);
            }
        }
    }
    Ok(())
}

fn load_state(path: &Path) -> Result<PersistedState> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn find_module_root(source: &Path) -> Result<PathBuf> {
    if source.join("module.toml").is_file() {
        return Ok(source.to_path_buf());
    }
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("could not read module source {}", source.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("module.toml").is_file() {
            return Ok(entry.path());
        }
    }
    bail!("no module.toml found in {}", source.display())
}

fn copy_module_tree(src: &Path, dst: &Path, include_git: bool) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    copy_module_tree_inner(src, dst, include_git, src)
}

fn copy_module_tree_inner(src: &Path, dst: &Path, include_git: bool, root: &Path) -> Result<()> {
    let at_root = src == root;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            if !include_git {
                continue;
            }
        } else if (at_root
            && matches!(
                name.to_string_lossy().as_ref(),
                "data" | "sidecar.log" | ".config" | ".cache" | ".local"
            ))
            || matches!(
                name.to_string_lossy().as_ref(),
                "target" | "node_modules" | ".venv" | "__pycache__"
            )
        {
            continue;
        }
        let target = dst.join(name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_module_tree_inner(&entry.path(), &target, include_git, root)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> Paths {
        let root = tempfile::tempdir().unwrap().keep();
        Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            runtime_dir: root.join("run"),
            config_file: root.join("config/config.toml"),
            state_file: root.join("data/state.json"),
            modules_dir: root.join("data/modules"),
            workshop_dir: root.join("data/workshop"),
            logs_dir: root.join("data/logs"),
        }
    }

    #[test]
    fn missing_state_does_not_grant_execution_permissions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/overview.html"), "card").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "test.permissions"
name = "Permissions"
version = "0.1.0"
api_version = "1"
overview = "ui/overview.html"
permissions = ["node.metrics", "backend", "media.record", "agent"]
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        let state = conservative_module_state(&manifest);
        assert!(state.granted_permissions.contains(&"node.metrics".into()));
        assert!(!state.granted_permissions.contains(&"backend".into()));
        assert!(!state.granted_permissions.contains(&"media.record".into()));
        assert!(!state.granted_permissions.contains(&"agent".into()));
        assert!(state.pending_permissions.contains(&"backend".into()));
    }

    #[tokio::test]
    async fn corrupt_state_fails_closed_and_preserves_the_file() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let root = paths.modules_dir.join("corrupt");
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::write(root.join("ui/overview.html"), "card").unwrap();
        std::fs::write(
            root.join("module.toml"),
            r#"schema = 1
id = "test.corrupt"
name = "Corrupt"
version = "0.1.0"
api_version = "1"
overview = "ui/overview.html"
permissions = ["node.metrics", "backend"]
"#,
        )
        .unwrap();
        std::fs::write(&paths.state_file, "{not json").unwrap();
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        let module = store.get("test.corrupt").unwrap();
        assert!(module.grants("node.metrics"));
        assert!(!module.grants("backend"));
        assert!(module.state.pending_permissions.contains(&"backend".into()));
        let backups = std::fs::read_dir(&paths.data_dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("state.corrupt.")
            })
            .count();
        assert_eq!(backups, 1);
    }

    #[tokio::test]
    async fn installs_and_updates_from_a_git_remote() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test.remote"
name = "Remote"
version = "0.1.0"
api_version = "1"
overview = "ui/overview.html"
"#,
        )
        .unwrap();
        std::fs::create_dir(source.path().join("ui")).unwrap();
        std::fs::write(source.path().join("ui/overview.html"), "one").unwrap();
        git::ensure_repo(source.path(), "initial").unwrap();
        let bare = tempfile::tempdir().unwrap();
        git::run(bare.path(), &["init", "--bare", "-b", "main"]).unwrap();
        let bare_url = format!("file://{}", bare.path().display());
        git::run(source.path(), &["remote", "add", "origin", &bare_url]).unwrap();
        git::run(source.path(), &["push", "-u", "origin", "main"]).unwrap();

        let store = ModuleStore::load(paths.clone()).await.unwrap();
        let installed = store.install_from_source(&bare_url, Some("main")).unwrap();
        assert_eq!(installed.manifest.id, "test.remote");
        assert!(installed.git.is_repo);
        assert!(!installed.git.dirty);
        assert_eq!(installed.git.repository.as_deref(), Some(bare_url.as_str()));

        std::fs::write(source.path().join("ui/overview.html"), "two").unwrap();
        std::fs::write(
            source.path().join("module.toml"),
            std::fs::read_to_string(source.path().join("module.toml"))
                .unwrap()
                .replace("0.1.0", "0.2.0"),
        )
        .unwrap();
        git::commit_all(source.path(), "update to two").unwrap();
        git::run(source.path(), &["push", "origin", "main"]).unwrap();

        let updated = store.update_from_git("test.remote").unwrap();
        assert_eq!(updated.manifest.version, "0.2.0");
        assert_eq!(
            std::fs::read_to_string(paths.module_dir("test.remote").join("ui/overview.html"))
                .unwrap(),
            "two"
        );
    }

    #[tokio::test]
    async fn checkpoint_commits_and_clears_dirty_state() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test.checkpoint"
name = "Checkpoint"
version = "0.1.0"
api_version = "1"
overview = "ui/overview.html"
"#,
        )
        .unwrap();
        std::fs::create_dir(source.path().join("ui")).unwrap();
        std::fs::write(source.path().join("ui/overview.html"), "one").unwrap();
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        store.install_from_path(source.path()).unwrap();
        let root = paths.module_dir("test.checkpoint");
        assert!(!git::status(&root).dirty);
        std::fs::write(root.join("ui/overview.html"), "two").unwrap();
        assert!(git::status(&root).dirty);
        store.checkpoint("test.checkpoint", "edit").unwrap();
        assert!(!git::status(&root).dirty);
    }

    #[tokio::test]
    async fn changing_sandbox_policy_revokes_the_backend_grant() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("ui")).unwrap();
        std::fs::write(source.path().join("ui/index.html"), "page").unwrap();
        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test.sandbox"
name = "Sandbox"
version = "0.1.0"
entry = "ui/index.html"
permissions = ["backend"]

[backend]
kind = "sidecar"
command = ["sh", "-c", "true"]
sandbox = true
"#,
        )
        .unwrap();
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        store.install_from_path(source.path()).unwrap();
        store
            .set_permissions("test.sandbox", vec!["backend".into()])
            .unwrap();

        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test.sandbox"
name = "Sandbox"
version = "0.2.0"
entry = "ui/index.html"
permissions = ["backend"]

[backend]
kind = "sidecar"
command = ["sh", "-c", "true"]
sandbox = false
"#,
        )
        .unwrap();
        let updated = store.install_from_path(source.path()).unwrap();
        assert!(!updated.granted_permissions.contains(&"backend".to_string()));
        assert!(updated.pending_permissions.contains(&"backend".to_string()));
    }

    #[tokio::test]
    async fn installs_a_local_module_and_preserves_data() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        assert!(store.get("system").is_none());
        assert!(store.get("opencode").is_none());

        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("ui")).unwrap();
        std::fs::write(source.path().join("ui/index.html"), "hi").unwrap();
        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test-module"
name = "Test"
version = "0.1.0"
entry = "ui/index.html"
permissions = []
"#,
        )
        .unwrap();
        let installed = store.install_from_path(source.path()).unwrap();
        assert_eq!(installed.manifest.id, "test-module");
        assert!(store.get("test-module").is_some());
        let data = paths.modules_dir.join("test-module/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("keep.txt"), "keep me").unwrap();

        std::fs::write(
            source.path().join("module.toml"),
            r#"schema = 1
id = "test-module"
name = "Test"
version = "0.2.0"
entry = "ui/index.html"
permissions = ["agent"]
"#,
        )
        .unwrap();
        let updated = store.install_from_path(source.path()).unwrap();
        assert_eq!(updated.manifest.version, "0.2.0");
        assert_eq!(
            std::fs::read_to_string(data.join("keep.txt")).unwrap(),
            "keep me"
        );
        assert!(updated.granted_permissions.is_empty());
        assert_eq!(updated.pending_permissions, vec!["agent".to_string()]);
        let granted = store
            .set_permissions("test-module", vec!["agent".to_string()])
            .unwrap();
        assert_eq!(granted.granted_permissions, vec!["agent".to_string()]);
        assert!(granted.pending_permissions.is_empty());
    }

    fn write_config_source(source: &Path) {
        std::fs::create_dir_all(source.join("ui")).unwrap();
        std::fs::write(source.join("ui/index.html"), "page").unwrap();
        std::fs::write(
            source.join("module.toml"),
            r#"schema = 1
id = "test.config"
name = "Config"
version = "0.1.0"
api_version = "1"
entry = "ui/index.html"

[[config.fields]]
key = "refresh_ms"
type = "number"
label = "Refresh"
default = 3000
min = 1000
max = 60000

[[config.fields]]
key = "mode"
type = "select"
label = "Mode"
default = "compact"
options = [
  { value = "compact", label = "Compact" },
  { value = "full", label = "Full" },
]
"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn enabled_state_and_config_survive_a_reload() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_config_source(source.path());
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        store.install_from_path(source.path()).unwrap();
        store.set_enabled("test.config", false).unwrap();

        let mut values = BTreeMap::new();
        values.insert("refresh_ms".into(), serde_json::json!(5000));
        values.insert("mode".into(), serde_json::json!("full"));
        let updated = store.set_config("test.config", values).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.config["refresh_ms"], 5000);
        assert_eq!(updated.config["mode"], "full");

        drop(store);
        let reloaded = ModuleStore::load(paths).await.unwrap();
        let module = reloaded.get("test.config").unwrap();
        assert!(!module.state.enabled);
        assert_eq!(module.state.config["refresh_ms"], 5000);
        assert_eq!(module.state.config["mode"], "full");
    }

    #[tokio::test]
    async fn config_validation_rejects_missing_unknown_and_invalid_values() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_config_source(source.path());
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        store.install_from_path(source.path()).unwrap();

        let mut missing = BTreeMap::new();
        missing.insert("refresh_ms".into(), serde_json::json!(3000));
        assert!(store.set_config("test.config", missing).is_err());

        let mut unknown = BTreeMap::new();
        unknown.insert("refresh_ms".into(), serde_json::json!(3000));
        unknown.insert("mode".into(), serde_json::json!("compact"));
        unknown.insert("extra".into(), serde_json::json!(true));
        assert!(store.set_config("test.config", unknown).is_err());

        for (refresh, mode) in [
            (serde_json::json!(10), serde_json::json!("compact")),
            (serde_json::json!("fast"), serde_json::json!("compact")),
            (serde_json::json!(3000), serde_json::json!("missing")),
        ] {
            let mut values = BTreeMap::new();
            values.insert("refresh_ms".into(), refresh);
            values.insert("mode".into(), mode);
            assert!(store.set_config("test.config", values).is_err());
        }
    }

    #[tokio::test]
    async fn permissions_must_be_declared_and_rescan_finds_new_checkouts() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_config_source(source.path());
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        store.install_from_path(source.path()).unwrap();
        assert!(
            store
                .set_permissions("test.config", vec!["backend".into()])
                .is_err()
        );

        let extra = paths.modules_dir.join("extra");
        std::fs::create_dir_all(extra.join("ui")).unwrap();
        std::fs::write(extra.join("ui/index.html"), "extra").unwrap();
        std::fs::write(
            extra.join("module.toml"),
            r#"schema = 1
id = "test.extra"
name = "Extra"
version = "0.1.0"
entry = "ui/index.html"
"#,
        )
        .unwrap();
        assert!(store.get("test.extra").is_none());
        store.rescan().unwrap();
        assert!(store.get("test.extra").is_some());
    }

    #[tokio::test]
    async fn invalid_sources_and_missing_modules_are_rejected() {
        let paths = temp_paths();
        paths.ensure().unwrap();
        let store = ModuleStore::load(paths.clone()).await.unwrap();
        let empty = tempfile::tempdir().unwrap();
        assert!(store.install_from_path(empty.path()).is_err());
        assert!(store.install_from_source("not-a-url", None).is_err());
        assert!(store.set_enabled("missing", false).is_err());
        assert!(store.remove("missing").is_err());
        assert!(store.set_config("missing", BTreeMap::new()).is_err());
        assert!(store.checkpoint("missing", "message").is_err());
    }
}
