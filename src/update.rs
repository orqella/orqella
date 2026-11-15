use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const UPDATE_MANIFEST: &str = "ORQELLA_UPDATE.json";
pub const UPDATE_SIGNATURE: &str = "ORQELLA_UPDATE.json.minisig";
const UPDATE_MANIFEST_SCHEMA: u32 = 1;
const MAX_UPDATE_MANIFEST_BYTES: usize = 64 * 1024;
const UPDATE_STATE_FILE: &str = "update-state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub schema: u32,
    pub version: String,
    pub commit: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone)]
struct VerifiedUpdate {
    manifest: UpdateManifest,
    version: Version,
    signed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppliedUpdate {
    version: String,
    commit: String,
    applied_at: String,
}

pub fn repo_path(config: &Config) -> Result<PathBuf> {
    if !config.update.repo.trim().is_empty() {
        return Ok(PathBuf::from(shellexpand(&config.update.repo)));
    }
    if let Ok(repo) = std::env::var("ORQELLA_REPO")
        && !repo.trim().is_empty()
    {
        return Ok(PathBuf::from(shellexpand(&repo)));
    }
    let exe = std::env::current_exe().context("could not locate the orqella binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    for ancestor in exe.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("target")
            && let Some(root) = ancestor.parent()
        {
            return Ok(root.to_path_buf());
        }
    }
    let cwd = std::env::current_dir().context("current directory unavailable")?;
    if cwd.join(".git").exists() {
        return Ok(cwd);
    }
    if let Some(home) = dirs::home_dir() {
        for candidate in [home.join("Projects/orqella/orqella"), home.join("orqella")] {
            if candidate.join(".git").exists() {
                return Ok(candidate);
            }
        }
    }
    bail!("no update repository configured; set [update] repo in the Orqella config")
}

fn shellexpand(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| value.to_string());
    }
    value.to_string()
}

fn git_command(repo: &Path) -> Command {
    let hooks = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", &format!("core.hooksPath={hooks}")])
        .args(["-c", "credential.helper="])
        .args(["-c", "protocol.ext.allow=never"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30");
    command
}

fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_command(repo)
        .args(args)
        .output()
        .with_context(|| format!("could not run git in {}", repo.display()))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { stdout } else { stderr }
        );
    }
    Ok(output.stdout)
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_bytes(repo, args)?)
        .trim()
        .to_string())
}

fn git_blob(repo: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("{rev}:{path}");
    let exists = git_command(repo)
        .args(["cat-file", "-e", &spec])
        .output()
        .with_context(|| format!("could not inspect git object in {}", repo.display()))?;
    if !exists.status.success() {
        return Ok(None);
    }
    Ok(Some(git_bytes(repo, &["cat-file", "blob", &spec])?))
}

fn configured_public_key(config: &Config) -> String {
    let configured = config.update.public_key.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    if let Ok(value) = std::env::var("ORQELLA_UPDATE_PUBLIC_KEY")
        && !value.trim().is_empty()
    {
        return value.trim().to_string();
    }
    option_env!("ORQELLA_UPDATE_PUBLIC_KEY")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn update_public_key(config: &Config) -> Result<PublicKey> {
    let value = configured_public_key(config);
    if value.is_empty() {
        bail!(
            "no update signing key is configured; set [update] public_key or build with ORQELLA_UPDATE_PUBLIC_KEY"
        );
    }
    let path = Path::new(&value);
    let key = if value.starts_with("untrusted comment:") {
        PublicKey::decode(&value)
    } else if value.contains(std::path::MAIN_SEPARATOR) && path.is_file() {
        PublicKey::from_file(path)
    } else {
        PublicKey::from_base64(&value)
    };
    key.context("invalid update signing public key")
}

fn verify_update_manifest_at(
    config: &Config,
    repo: &Path,
    rev: &str,
) -> Result<Option<VerifiedUpdate>> {
    let Some(bytes) = git_blob(repo, rev, UPDATE_MANIFEST)? else {
        return Ok(None);
    };
    if bytes.len() > MAX_UPDATE_MANIFEST_BYTES {
        bail!("update manifest is larger than {MAX_UPDATE_MANIFEST_BYTES} bytes");
    }
    let manifest: UpdateManifest =
        serde_json::from_slice(&bytes).context("update manifest is not valid JSON")?;
    if manifest.schema != UPDATE_MANIFEST_SCHEMA {
        bail!(
            "unsupported update manifest schema {}; expected {UPDATE_MANIFEST_SCHEMA}",
            manifest.schema
        );
    }
    if manifest.commit.len() < 7
        || manifest.commit.len() > 64
        || !manifest.commit.chars().all(|c| c.is_ascii_hexdigit())
    {
        bail!("update manifest has an invalid commit id");
    }
    if manifest.commit == rev {
        bail!("update manifest cannot describe the commit that contains it");
    }
    let ancestor = git_command(repo)
        .args(["merge-base", "--is-ancestor", &manifest.commit, rev])
        .status()
        .with_context(|| format!("could not inspect git history in {}", repo.display()))?;
    if !ancestor.success() {
        bail!(
            "update manifest commit {} is not an ancestor of the fetched commit {rev}",
            manifest.commit
        );
    }
    let changed = git(repo, &["diff", "--name-only", &manifest.commit, rev, "--"])?;
    if changed
        .lines()
        .any(|path| path != UPDATE_MANIFEST && path != UPDATE_SIGNATURE)
    {
        bail!(
            "the fetched commit contains changes after the signed release commit {}",
            manifest.commit
        );
    }
    let version = Version::parse(&manifest.version)
        .with_context(|| format!("invalid release version {:?}", manifest.version))?;
    let Some(signature_bytes) = git_blob(repo, rev, UPDATE_SIGNATURE)? else {
        if config.update.allow_unsigned {
            return Ok(Some(VerifiedUpdate {
                manifest,
                version,
                signed: false,
            }));
        }
        bail!("update manifest is unsigned; refusing to execute it");
    };
    let signature_text =
        std::str::from_utf8(&signature_bytes).context("update signature is not UTF-8")?;
    let signature =
        Signature::decode(signature_text).context("update manifest signature is malformed")?;
    let trusted = signature.trusted_comment();
    if !trusted.contains(&format!("version:{}", manifest.version))
        || !trusted.contains(&format!("commit:{}", manifest.commit))
    {
        bail!("update manifest signature does not cover its version and commit");
    }
    update_public_key(config)?
        .verify(&bytes, &signature, false)
        .context("update manifest signature did not verify")?;
    Ok(Some(VerifiedUpdate {
        manifest,
        version,
        signed: true,
    }))
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn applied_update(paths: &Paths) -> Option<AppliedUpdate> {
    let raw = std::fs::read_to_string(paths.data_dir.join(UPDATE_STATE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn record_applied(paths: &Paths, manifest: &UpdateManifest) -> Result<()> {
    let value = AppliedUpdate {
        version: manifest.version.clone(),
        commit: manifest.commit.clone(),
        applied_at: crate::util::now_iso(),
    };
    let raw = serde_json::to_vec_pretty(&value)?;
    let path = paths.data_dir.join(UPDATE_STATE_FILE);
    crate::util::write_atomic(&path, &raw)?;
    let _ = crate::util::restrict_file(&path);
    Ok(())
}

fn enforce_forward_update(paths: &Paths, update: &VerifiedUpdate) -> Result<()> {
    let current = current_version();
    if update.version <= current {
        bail!(
            "release {} is not newer than the running version {current}",
            update.version
        );
    }
    if let Some(applied) = applied_update(paths)
        && let Ok(version) = Version::parse(&applied.version)
        && update.version <= version
    {
        bail!(
            "release {} is not newer than the last applied update {version} (rollback refused)",
            update.version
        );
    }
    Ok(())
}

pub fn check(config: &Config, paths: &Paths) -> Result<Value> {
    let repo = repo_path(config)?;
    if !repo.join(".git").exists() {
        bail!(
            "update repository is not a git checkout: {}",
            repo.display()
        );
    }
    let remote = &config.update.remote;
    let branch = &config.update.branch;
    git(&repo, &["fetch", "--quiet", remote, branch])?;
    let current = git(&repo, &["rev-parse", "HEAD"])?;
    let latest = git(&repo, &["rev-parse", &format!("{remote}/{branch}")])?;
    let status = git(&repo, &["status", "--porcelain"])?;
    let behind = git(
        &repo,
        &["rev-list", "--count", &format!("HEAD..{remote}/{branch}")],
    )?
    .parse::<u64>()
    .unwrap_or(0);
    let mut value = json!({
        "ok": true,
        "repo": repo,
        "remote": remote,
        "branch": branch,
        "current": current,
        "latest": latest,
        "current_short": &current[..current.len().min(8)],
        "latest_short": &latest[..latest.len().min(8)],
        "behind": behind,
        "available": behind > 0,
        "dirty": !status.is_empty(),
        "signed": false,
    });
    if behind == 0 {
        return Ok(value);
    }
    match verify_update_manifest_at(config, &repo, &latest) {
        Ok(Some(update)) => {
            value["signed"] = json!(update.signed);
            value["version"] = json!(update.manifest.version);
            value["manifest"] = json!(update.manifest);
            if let Err(error) = enforce_forward_update(paths, &update) {
                value["ok"] = json!(false);
                value["available"] = json!(false);
                value["error"] = json!(format!("{error:#}"));
            }
        }
        Ok(None) => {
            if config.update.allow_unsigned {
                value["available"] = json!(true);
            } else {
                value["ok"] = json!(false);
                value["available"] = json!(false);
                value["error"] =
                    json!("no signed update manifest was found; refuse to execute this checkout");
            }
        }
        Err(error) => {
            value["ok"] = json!(false);
            value["available"] = json!(false);
            value["error"] = json!(format!("{error:#}"));
        }
    }
    Ok(value)
}

pub fn status(paths: &Paths) -> Value {
    let path = paths.data_dir.join("update.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return json!({ "status": "idle" });
    };
    serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "status": "idle" }))
}

fn write_status(paths: &Paths, value: &Value) {
    let path = paths.data_dir.join("update.json");
    if let Ok(raw) = serde_json::to_vec_pretty(value) {
        let _ = crate::util::write_atomic(&path, &raw);
    }
}

pub fn in_progress(paths: &Paths) -> bool {
    let value = status(paths);
    if !matches!(
        value.get("status").and_then(Value::as_str),
        Some("queued" | "running")
    ) {
        return false;
    }
    let Some(updated) = value
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    else {
        return true;
    };
    (chrono::Utc::now() - updated.with_timezone(&chrono::Utc)) < chrono::Duration::hours(1)
}

pub fn mark_failed(paths: &Paths, error: &str) {
    write_status(
        paths,
        &status_value("failed", "failed", None, &format!("{error}\n")),
    );
}

fn status_value(status: &str, phase: &str, repo: Option<&Path>, log: &str) -> Value {
    let mut value = json!({
        "status": status,
        "phase": phase,
        "log": log,
        "updated_at": crate::util::now_iso(),
    });
    if let Some(repo) = repo {
        value["repo"] = json!(repo);
    }
    value
}

fn log_line(paths: &Paths, value: &Value, line: &str) -> Value {
    let mut value = value.clone();
    let old = value.get("log").and_then(Value::as_str).unwrap_or("");
    value["log"] = json!(
        format!("{old}{line}\n")
            .chars()
            .rev()
            .take(30000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );
    write_status(paths, &value);
    value
}

pub fn apply(config: &Config, paths: &Paths, parent_pid: Option<u32>) -> Result<()> {
    let repo = repo_path(config)?;
    let mut state = status_value("running", "checking", Some(&repo), "");
    write_status(paths, &state);

    let result = apply_inner(config, paths, &repo, parent_pid, &mut state);
    match result {
        Ok(()) => {
            state["status"] = json!("succeeded");
            state["phase"] = json!("complete");
            state["updated_at"] = json!(crate::util::now_iso());
            write_status(paths, &state);
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            state = log_line(paths, &state, &message);
            state["status"] = json!("failed");
            state["phase"] = json!("failed");
            state["error"] = json!(message);
            state["updated_at"] = json!(crate::util::now_iso());
            write_status(paths, &state);
            Err(error)
        }
    }
}

fn apply_inner(
    config: &Config,
    paths: &Paths,
    repo: &Path,
    parent_pid: Option<u32>,
    state: &mut Value,
) -> Result<()> {
    if !repo.join(".git").exists() {
        bail!(
            "update repository is not a git checkout: {}",
            repo.display()
        );
    }
    let dirty = git(repo, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        bail!("the update checkout has local changes; commit or stash them first");
    }
    let remote = &config.update.remote;
    let branch = &config.update.branch;
    *state = log_line(paths, state, &format!("Fetching {remote}/{branch}…"));
    git(repo, &["fetch", "--quiet", remote, branch])?;
    let current = git(repo, &["rev-parse", "HEAD"])?;
    let latest = git(repo, &["rev-parse", &format!("{remote}/{branch}")])?;
    let behind = git(
        repo,
        &["rev-list", "--count", &format!("HEAD..{remote}/{branch}")],
    )?
    .parse::<u64>()
    .unwrap_or(0);
    state["current"] = json!(current);
    state["latest"] = json!(latest);
    state["behind"] = json!(behind);
    if behind == 0 {
        *state = log_line(paths, state, "Already at or ahead of the latest commit.");
        return Ok(());
    }
    let verified = match verify_update_manifest_at(config, repo, &latest) {
        Ok(Some(update)) => {
            enforce_forward_update(paths, &update)?;
            *state = log_line(
                paths,
                state,
                &format!(
                    "Verified release {} ({})",
                    update.manifest.version,
                    if update.signed { "signed" } else { "unsigned" }
                ),
            );
            state["signed"] = json!(update.signed);
            state["version"] = json!(update.manifest.version);
            Some(update)
        }
        Ok(None) if config.update.allow_unsigned => {
            *state = log_line(
                paths,
                state,
                "No signed update manifest; continuing because [update] allow_unsigned is set.",
            );
            None
        }
        Ok(None) => {
            bail!("the fetched commit has no signed update manifest; refusing to execute it")
        }
        Err(error) => return Err(error),
    };
    *state = log_line(
        paths,
        state,
        &format!("Checking out {}…", &latest[..latest.len().min(8)]),
    );
    // Merge the exact commit whose manifest was verified. `git pull` would
    // fetch again and could fast-forward past the verified commit.
    git(repo, &["merge", "--ff-only", &latest])?;
    let head = git(repo, &["rev-parse", "HEAD"])?;
    if head != latest {
        bail!(
            "checkout is at {head}, not the verified commit {latest}; refusing to run the installer"
        );
    }
    if verified.is_some() {
        // The checked-out tree must still carry the verified manifest and
        // signature; this also catches a merge hook or filter that changed it.
        if verify_update_manifest_at(config, repo, &head)?.is_none() {
            bail!("the checked-out tree no longer contains the verified update manifest");
        }
    }

    let installer = installer_path(config, repo);
    let Some(installer) = installer else {
        bail!(
            "no installer script found; expected install-macos.sh, install-linux.sh, or install-windows.ps1"
        );
    };
    if !config.update.allow_unsigned {
        let repo_root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        let installer_path = installer
            .canonicalize()
            .unwrap_or_else(|_| installer.clone());
        if !installer_path.starts_with(&repo_root) {
            bail!(
                "installer {} is outside the verified checkout {}; refusing to run it",
                installer.display(),
                repo.display()
            );
        }
    }
    *state = log_line(paths, state, &format!("Running {}", installer.display()));
    let log_path = paths.data_dir.join("update.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("could not open {}", log_path.display()))?;
    let _ = crate::util::restrict_file(&log_path);
    let log_err = log.try_clone()?;
    let mut command = if installer.extension().and_then(|ext| ext.to_str()) == Some("ps1") {
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
        command.arg(&installer);
        command
    } else {
        let mut command = Command::new("bash");
        command.arg(&installer);
        command
    };
    command
        .current_dir(repo)
        .env("ORQELLA_UPDATE_REPO", repo)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if let Some(pid) = parent_pid {
        command.env("ORQELLA_UPDATE_PARENT_PID", pid.to_string());
    }
    let status = command.status().context("could not run installer")?;
    if !status.success() {
        bail!("installer exited with {status}; see {}", log_path.display());
    }
    if let Some(update) = verified
        && let Err(error) = record_applied(paths, &update.manifest)
    {
        *state = log_line(
            paths,
            state,
            &format!("Warning: could not record the applied release: {error:#}"),
        );
    }
    *state = log_line(paths, state, "Installer completed.");
    Ok(())
}

fn installer_path(config: &Config, repo: &Path) -> Option<PathBuf> {
    if !config.update.installer.trim().is_empty() {
        let path = PathBuf::from(shellexpand(&config.update.installer));
        let path = if path.is_absolute() {
            path
        } else {
            repo.join(path)
        };
        if path.is_file() {
            return Some(path);
        }
    }
    let names = if cfg!(target_os = "macos") {
        vec!["install-macos.sh"]
    } else if cfg!(target_os = "windows") {
        vec!["install-windows.ps1"]
    } else {
        vec!["install-linux.sh"]
    };
    names
        .into_iter()
        .map(|name| repo.join(name))
        .find(|path| path.is_file())
}

pub fn spawn(config: &Config, paths: &Paths, parent_pid: u32) -> Result<()> {
    let repo = repo_path(config)?;
    write_status(paths, &status_value("queued", "starting", Some(&repo), ""));
    let exe = std::env::current_exe().context("could not locate the orqella binary")?;
    let parent = parent_pid.to_string();
    #[cfg(target_os = "linux")]
    {
        if let Some(systemd_run) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("systemd-run"))
                .find(|candidate| candidate.is_file())
        }) {
            let unit = format!("orqella-update-{}", parent_pid);
            let path =
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
            let status = Command::new(systemd_run)
                .args(["--user", "--collect", "--quiet", "--unit", &unit])
                .arg(format!("--setenv=PATH={path}"))
                .arg(&exe)
                .args(["update", "apply", "--parent-pid", &parent])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|status| status.success()) {
                return Ok(());
            }
        }
    }
    let mut command = Command::new(exe);
    command
        .args(["update", "apply", "--parent-pid", &parent])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    command.spawn().context("could not start updater")?;
    Ok(())
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use base64::Engine;
    use blake2::{Blake2b512, Digest};
    use ed25519_dalek::{Signer, SigningKey};

    fn git_test(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn sign_manifest(manifest: &[u8], key: &SigningKey, key_id: [u8; 8], trusted: &str) -> String {
        let digest = Blake2b512::digest(manifest);
        let signature = key.sign(&digest);
        let mut signature_bin = vec![b'E', b'D'];
        signature_bin.extend_from_slice(&key_id);
        signature_bin.extend_from_slice(&signature.to_bytes());
        let mut global_input = signature.to_bytes().to_vec();
        global_input.extend_from_slice(trusted.as_bytes());
        let global = key.sign(&global_input);
        let encoder = base64::engine::general_purpose::STANDARD;
        format!(
            "untrusted comment: test\n{}\ntrusted comment: {trusted}\n{}",
            encoder.encode(signature_bin),
            encoder.encode(global.to_bytes())
        )
    }

    fn public_key_base64(key: &SigningKey, key_id: [u8; 8]) -> String {
        let mut bin = vec![b'E', b'd'];
        bin.extend_from_slice(&key_id);
        bin.extend_from_slice(key.verifying_key().as_bytes());
        base64::engine::general_purpose::STANDARD.encode(bin)
    }

    fn paths(root: &Path) -> Paths {
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
    fn status_defaults_to_idle_and_tolerates_corrupt_state() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        assert_eq!(status(&paths)["status"], "idle");
        std::fs::write(paths.data_dir.join("update.json"), "{not json").unwrap();
        assert_eq!(status(&paths)["status"], "idle");
    }

    #[test]
    fn in_progress_expires_stale_queued_and_running_records() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let state_file = paths.data_dir.join("update.json");
        std::fs::write(
            &state_file,
            json!({"status": "running", "updated_at": crate::util::now_iso()}).to_string(),
        )
        .unwrap();
        assert!(in_progress(&paths));
        std::fs::write(&state_file, json!({"status": "running"}).to_string()).unwrap();
        assert!(in_progress(&paths));
        std::fs::write(
            &state_file,
            json!({"status": "running", "updated_at": "2000-01-01T00:00:00Z"}).to_string(),
        )
        .unwrap();
        assert!(!in_progress(&paths));
        std::fs::write(&state_file, json!({"status": "succeeded"}).to_string()).unwrap();
        assert!(!in_progress(&paths));
    }

    #[test]
    fn mark_failed_records_a_visible_error_and_log() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        mark_failed(&paths, "installer exploded");
        let value = status(&paths);
        assert_eq!(value["status"], "failed");
        assert_eq!(value["phase"], "failed");
        assert!(
            value["log"]
                .as_str()
                .unwrap()
                .contains("installer exploded")
        );
        assert!(value["updated_at"].as_str().is_some());
    }

    #[test]
    fn repo_path_prefers_explicit_configuration_and_expands_home() {
        let mut config = Config::default();
        config.update.repo = "~/somewhere/orqella".into();
        let path = repo_path(&config).unwrap();
        assert!(path.ends_with("somewhere/orqella"));
        assert!(!path.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn installer_path_prefers_the_configured_script() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("custom-install.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let mut config = Config::default();
        config.update.installer = script.to_string_lossy().to_string();
        assert_eq!(installer_path(&config, root.path()), Some(script));

        config.update.installer = "missing.sh".into();
        assert_eq!(installer_path(&config, root.path()), None);
        let expected = if cfg!(target_os = "macos") {
            "install-macos.sh"
        } else if cfg!(target_os = "windows") {
            "install-windows.ps1"
        } else {
            "install-linux.sh"
        };
        std::fs::write(root.path().join(expected), "").unwrap();
        assert_eq!(
            installer_path(&config, root.path()),
            Some(root.path().join(expected))
        );
    }

    #[test]
    fn signed_manifest_must_cover_the_release_commit() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path();
        git_test(repo, &["init", "-q"]);
        git_test(repo, &["config", "user.email", "test@example.com"]);
        git_test(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "release\n").unwrap();
        git_test(repo, &["add", "README.md"]);
        git_test(repo, &["commit", "-q", "-m", "release"]);
        let release = git_test(repo, &["rev-parse", "HEAD"]);

        let key = SigningKey::from_bytes(&[9u8; 32]);
        let key_id = [1, 2, 3, 4, 5, 6, 7, 8];
        let manifest = serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "version": "9.9.9",
            "commit": release,
            "channel": "test",
        }))
        .unwrap();
        let trusted = format!("version:9.9.9 commit:{release}");
        let signature = sign_manifest(&manifest, &key, key_id, &trusted);
        std::fs::write(repo.join(UPDATE_MANIFEST), &manifest).unwrap();
        std::fs::write(repo.join(UPDATE_SIGNATURE), signature).unwrap();
        git_test(repo, &["add", UPDATE_MANIFEST, UPDATE_SIGNATURE]);
        git_test(repo, &["commit", "-q", "-m", "sign release"]);
        let signed = git_test(repo, &["rev-parse", "HEAD"]);

        let mut config = Config::default();
        config.update.public_key = public_key_base64(&key, key_id);
        let verified = verify_update_manifest_at(&config, repo, &signed)
            .unwrap()
            .expect("signed manifest");
        assert!(verified.signed);
        assert_eq!(verified.version, Version::new(9, 9, 9));
        assert!(
            verify_update_manifest_at(&config, repo, &release)
                .unwrap()
                .is_none()
        );

        std::fs::write(repo.join("README.md"), "unsigned follow-up\n").unwrap();
        git_test(repo, &["add", "README.md"]);
        git_test(repo, &["commit", "-q", "-m", "unsigned follow-up"]);
        let changed = git_test(repo, &["rev-parse", "HEAD"]);
        assert!(verify_update_manifest_at(&config, repo, &changed).is_err());
    }

    #[test]
    fn rollback_protection_uses_the_high_water_mark() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let manifest = UpdateManifest {
            schema: 1,
            version: "0.3.1".into(),
            commit: "a".repeat(40),
            channel: "test".into(),
            published_at: String::new(),
            notes: String::new(),
        };
        let update = VerifiedUpdate {
            version: Version::new(0, 3, 1),
            manifest: manifest.clone(),
            signed: true,
        };
        assert!(enforce_forward_update(&paths, &update).is_ok());

        record_applied(
            &paths,
            &UpdateManifest {
                version: "0.4.0".into(),
                ..manifest
            },
        )
        .unwrap();
        assert!(enforce_forward_update(&paths, &update).is_err());
    }

    #[test]
    fn log_lines_are_capped_to_the_most_recent_characters() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let value = status_value("running", "test", None, "");
        log_line(&paths, &value, &"x".repeat(40_000));
        let stored = status(&paths);
        assert_eq!(stored["log"].as_str().unwrap().chars().count(), 30_000);
        assert!(stored["log"].as_str().unwrap().ends_with('\n'));
    }

    #[test]
    fn apply_rejects_a_checkout_that_is_not_git_and_records_failure() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let mut config = Config::default();
        config.update.repo = root.path().join("checkout").to_string_lossy().to_string();
        std::fs::create_dir_all(&config.update.repo).unwrap();
        let error = apply(&config, &paths, None).unwrap_err();
        assert!(format!("{error:#}").contains("not a git checkout"));
        let value = status(&paths);
        assert_eq!(value["status"], "failed");
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .contains("not a git checkout")
        );
    }
}
