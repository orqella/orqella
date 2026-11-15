//! Git-backed module checkouts.
//!
//! Every installed module lives in its own git checkout. The daemon does not
//! embed the repository contents inside the Orqella source tree: a module is a
//! repository that can be developed and updated independently.

use crate::util;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

const GITIGNORE: &str = "\
# Orqella runtime data. Never publish module state or caches.
/data/
.venv/
node_modules/
target/
__pycache__/
/sidecar.log
/.config/
/.cache/
/.local/
";

pub const DEFAULT_BRANCH: &str = "main";

#[derive(Debug, Clone, Default)]
pub struct RepoStatus {
    pub is_repo: bool,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

fn safe_hooks_path() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn safe_git_args(args: &[&str]) -> Vec<String> {
    let hooks = safe_hooks_path();
    let mut values = vec![
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
        "-c".to_string(),
        format!("core.hooksPath={hooks}"),
        "-c".to_string(),
        "credential.helper=".to_string(),
        "-c".to_string(),
        "protocol.ext.allow=never".to_string(),
    ];
    values.extend(args.iter().map(|value| value.to_string()));
    values
}

pub fn run(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(safe_git_args(args))
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .output()
        .with_context(|| format!("could not run git in {}", root.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run_allow_failure(root: &Path, args: &[&str]) -> Option<String> {
    run(root, args).ok()
}

pub fn is_repo(root: &Path) -> bool {
    root.join(".git").exists()
}

pub fn status(root: &Path) -> RepoStatus {
    if !is_repo(root) {
        return RepoStatus::default();
    }
    let repository = run_allow_failure(root, &["config", "--get", "remote.origin.url"])
        .filter(|value| !value.is_empty());
    let branch = run_allow_failure(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|value| !value.is_empty() && value != "HEAD");
    let commit = run_allow_failure(root, &["rev-parse", "HEAD"]).filter(|value| !value.is_empty());
    let dirty = run_allow_failure(root, &["status", "--porcelain"])
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let upstream = run_allow_failure(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .filter(|value| !value.is_empty());
    let (ahead, behind) = upstream
        .as_deref()
        .and_then(|_| {
            run_allow_failure(
                root,
                &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
            )
        })
        .and_then(|value| {
            let mut parts = value.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .unwrap_or((0, 0));
    RepoStatus {
        is_repo: true,
        repository,
        branch,
        commit,
        dirty,
        upstream,
        ahead,
        behind,
    }
}

pub fn short_commit(commit: &str) -> String {
    commit.chars().take(12).collect()
}

pub fn ensure_gitignore(root: &Path) -> Result<bool> {
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut missing = Vec::new();
    for line in GITIGNORE.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || existing.lines().any(|item| item.trim() == line)
        {
            continue;
        }
        missing.push(line);
    }
    if missing.is_empty() {
        return Ok(false);
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("\n# Added by Orqella\n");
    for line in missing {
        next.push_str(line);
        next.push('\n');
    }
    util::write_atomic(&path, next.as_bytes())?;
    Ok(true)
}

/// Initialize a repository and commit the current tree when the module does not
/// have git metadata yet. Existing repositories are left untouched so the
/// author keeps control of history.
pub fn ensure_repo(root: &Path, message: &str) -> Result<bool> {
    if !util::git_available() {
        bail!("git is not installed; modules require git for updates");
    }
    if is_repo(root) {
        // Keep runtime data out of an existing checkout without creating a
        // local commit that would make the next `git pull` diverge.
        ensure_local_excludes(root)?;
        return Ok(false);
    }
    ensure_gitignore(root)?;
    run(root, &["init", "-b", DEFAULT_BRANCH])
        .or_else(|_| run(root, &["init"]))
        .context("could not initialize the module repository")?;
    commit_all(root, message)?;
    Ok(true)
}

/// Ignore runtime paths through the repository's local exclude file. This does
/// not alter tracked files, so a cloned module can still fast-forward.
pub fn ensure_local_excludes(root: &Path) -> Result<()> {
    if !root.join(".git").is_dir() {
        bail!("module is not a git repository");
    }
    let path = root.join(".git/info/exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut next = existing;
    let mut changed = false;
    for line in GITIGNORE.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if next.lines().any(|item| item.trim() == line) {
            continue;
        }
        if !changed {
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str("# Added by Orqella\n");
            changed = true;
        }
        next.push_str(line);
        next.push('\n');
    }
    if changed {
        util::write_atomic(&path, next.as_bytes())?;
    }
    Ok(())
}

pub fn commit_all(root: &Path, message: &str) -> Result<bool> {
    if !is_repo(root) {
        bail!("module is not a git repository");
    }
    git_identity(root, &["add", "-A"])?;
    let staged = run_allow_failure(root, &["diff", "--cached", "--quiet"]);
    if staged.is_some() {
        // `git diff --cached --quiet` exits 0 when there are no changes.
        return Ok(false);
    }
    git_identity(root, &["commit", "--no-verify", "-m", message])?;
    Ok(true)
}

pub fn clone(url: &str, branch: Option<&str>, destination: &Path) -> Result<()> {
    if !util::git_available() {
        bail!("git is not installed; cannot install a module from a repository");
    }
    if destination.exists() {
        bail!("destination already exists: {}", destination.display());
    }
    let mut command = Command::new("git");
    command
        .args(safe_git_args(&[]))
        .arg("clone")
        .arg("--no-tags");
    if let Some(branch) = branch {
        command.arg("--branch").arg(branch);
    }
    command
        .arg(url)
        .arg(destination)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .with_context(|| format!("could not clone {url}"))?;
    if !output.status.success() {
        bail!(
            "could not clone {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub fn pull(root: &Path) -> Result<()> {
    if !is_repo(root) {
        bail!("module is not a git repository");
    }
    let status = status(root);
    if status.dirty {
        bail!("module has uncommitted changes; commit them before updating");
    }
    if status.upstream.is_none() {
        bail!("module repository has no upstream branch; set an origin and upstream first");
    }
    run(root, &["pull", "--ff-only"])
        .map(|_| ())
        .context("could not update the module from its origin")
}

fn git_identity(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(safe_git_args(&[
            "-c",
            "user.name=Orqella",
            "-c",
            "user.email=orqella@localhost",
        ]))
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .output()
        .with_context(|| format!("could not run git in {}", root.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub fn is_remote(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("ssh://")
        || source.starts_with("git@")
        || source.starts_with("git://")
        || source.starts_with("file://")
}

pub fn github_repository(url: &str) -> Option<String> {
    let url = url.trim();
    let tail = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        url.strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
            .or_else(|| url.strip_prefix("ssh://git@github.com/"))?
    };
    let tail = tail.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = tail.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_github_remotes() {
        assert_eq!(
            github_repository("git@github.com:acme/weather.git").as_deref(),
            Some("acme/weather")
        );
        assert_eq!(
            github_repository("https://github.com/acme/weather.git").as_deref(),
            Some("acme/weather")
        );
        assert_eq!(
            github_repository("https://gitlab.com/acme/weather.git"),
            None
        );
    }

    #[test]
    fn keeps_module_data_out_of_git() {
        let root = tempfile::tempdir().unwrap();
        assert!(ensure_gitignore(root.path()).unwrap());
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert!(content.contains("/data/"));
        std::fs::create_dir(root.path().join("data")).unwrap();
        std::fs::write(root.path().join("data/state.json"), "{}").unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "ok").unwrap();
        std::fs::create_dir_all(root.path().join(".config/pulse")).unwrap();
        std::fs::write(root.path().join(".config/pulse/cookie"), "secret").unwrap();
        ensure_repo(root.path(), "initial").unwrap();
        let tracked = run(root.path(), &["ls-files"]).unwrap();
        assert!(tracked.contains("ui/index.html"));
        assert!(!tracked.contains("data/state.json"));
        assert!(!tracked.contains(".config/pulse/cookie"));
    }

    #[test]
    fn daemon_git_operations_ignore_repo_hooks_and_fsmonitor() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), "one").unwrap();
        ensure_repo(root.path(), "initial").unwrap();

        let marker = root.path().join("hook-ran");
        let hook = root.path().join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(root.path().join("file.txt"), "two").unwrap();
        assert!(commit_all(root.path(), "second").unwrap());
        assert!(!marker.exists(), "a module hook must not run");

        let fsmonitor = root.path().join("fsmonitor.sh");
        std::fs::write(
            &fsmonitor,
            format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fsmonitor, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let configured = std::process::Command::new("git")
            .args(["config", "core.fsmonitor"])
            .arg(&fsmonitor)
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(configured.success());
        let _ = status(root.path());
        assert!(!marker.exists(), "core.fsmonitor must not run");
    }

    #[test]
    fn classifies_remote_sources_without_accepting_local_paths() {
        for source in [
            "https://github.com/acme/weather.git",
            "http://example.com/acme/weather.git",
            "ssh://git@example.com/acme/weather.git",
            "git@github.com:acme/weather.git",
            "file:///tmp/weather",
        ] {
            assert!(is_remote(source), "{source}");
        }
        for source in ["", "acme/weather", "/tmp/weather", "C:\\weather"] {
            assert!(!is_remote(source), "{source}");
        }
    }

    #[test]
    fn short_commits_never_panic_on_short_input() {
        assert_eq!(short_commit(""), "");
        assert_eq!(short_commit("abc"), "abc");
        assert_eq!(short_commit("0123456789abcdef"), "0123456789ab");
    }

    #[test]
    fn gitignore_is_created_once_and_preserves_user_rules() {
        let root = tempfile::tempdir().unwrap();
        assert!(ensure_gitignore(root.path()).unwrap());
        assert!(!ensure_gitignore(root.path()).unwrap());
        let current = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        std::fs::write(
            root.path().join(".gitignore"),
            format!("{current}\n# custom\nlocal.txt\n"),
        )
        .unwrap();
        assert!(!ensure_gitignore(root.path()).unwrap());
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert!(content.contains("# custom"));
        assert!(content.contains("local.txt"));
        assert_eq!(content.matches("/data/").count(), 1);
    }
}
