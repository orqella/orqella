use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .output()
        .with_context(|| format!("could not run git in {}", repo.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { stdout } else { stderr }
        );
    }
    Ok(stdout)
}

pub fn check(config: &Config) -> Result<Value> {
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
    Ok(json!({
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
    }))
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
    *state = log_line(
        paths,
        state,
        &format!("Pulling {}…", &latest[..latest.len().min(8)]),
    );
    git(repo, &["pull", "--ff-only", remote, branch])?;

    let installer = installer_path(config, repo);
    let Some(installer) = installer else {
        bail!(
            "no installer script found; expected install-macos.sh, install-linux.sh, or install-windows.ps1"
        );
    };
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
