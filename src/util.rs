use anyhow::{Context, Result, bail};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("could not write temporary file {}", tmp.display()))?;
    file.write_all(data)
        .with_context(|| format!("could not write temporary file {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("could not sync temporary file {}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
pub fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn restrict_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn restrict_dir(_path: &Path) -> Result<()> {
    Ok(())
}

fn ensure_regular_file(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(())
}

/// Open a file for appending without following a symlink at the final path.
pub fn open_append_no_follow(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    ensure_regular_file(&file, path)?;
    Ok(file)
}

/// Open a file for reading without following a symlink at the final path.
pub fn open_read_no_follow(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    ensure_regular_file(&file, path)?;
    Ok(file)
}

pub fn free_port() -> Result<u16> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).context("could not allocate a loopback port")?;
    Ok(listener.local_addr()?.port())
}

pub fn safe_join(root: &Path, requested: &str) -> Option<PathBuf> {
    let rel = Path::new(requested);
    if rel.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    let joined = root.join(clean);
    let canonical_root = root.canonicalize().ok()?;
    let canonical_joined = joined.canonicalize().ok()?;
    if canonical_joined.starts_with(&canonical_root) {
        Some(canonical_joined)
    } else {
        None
    }
}

pub fn init_git_repo(path: &Path) -> bool {
    if !git_available() {
        return false;
    }
    let ok = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or_else(|_| {
            std::process::Command::new("git")
                .arg("init")
                .current_dir(path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        });
    ok && git_commit(path, "Orqella module scaffold")
}

fn safe_git_config() -> Vec<String> {
    let hooks = if cfg!(windows) { "NUL" } else { "/dev/null" };
    vec![
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        format!("core.hooksPath={hooks}"),
        "-c".into(),
        "credential.helper=".into(),
        "-c".into(),
        "protocol.ext.allow=never".into(),
    ]
}

pub fn git_commit(path: &Path, message: &str) -> bool {
    if !git_available() {
        return false;
    }
    let mut add_args = safe_git_config();
    add_args.extend([
        "-c".to_string(),
        "user.name=Orqella".to_string(),
        "-c".to_string(),
        "user.email=orqella@localhost".to_string(),
        "add".to_string(),
        "-A".to_string(),
    ]);
    let status = std::process::Command::new("git")
        .args(&add_args)
        .current_dir(path)
        .status();
    if !status.map(|status| status.success()).unwrap_or(false) {
        return false;
    }
    let mut commit_args = safe_git_config();
    commit_args.extend([
        "-c".to_string(),
        "user.name=Orqella".to_string(),
        "-c".to_string(),
        "user.email=orqella@localhost".to_string(),
        "commit".to_string(),
        "--no-verify".to_string(),
        "-m".to_string(),
        message.to_string(),
    ]);
    std::process::Command::new("git")
        .args(&commit_args)
        .current_dir(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

pub fn validate_module_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 96 {
        bail!("module id must be between 1 and 96 characters");
    }
    let first = id.chars().next().unwrap_or('-');
    if !first.is_ascii_alphanumeric() {
        bail!("module id must start with a letter or digit");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("module id may only contain letters, digits, '.', '_' and '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_ids_reject_paths() {
        assert!(validate_module_id("com.example.thing").is_ok());
        assert!(validate_module_id("../escape").is_err());
        assert!(validate_module_id("").is_err());
        assert!(validate_module_id("-bad").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_writes_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("secret.toml");
        write_atomic(&path, b"token = 'x'").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_open_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("outside");
        let link = root.path().join("link");
        std::fs::write(&target, b"secret").unwrap();
        symlink(&target, &link).unwrap();
        assert!(open_read_no_follow(&link).is_err());
        assert!(open_append_no_follow(&link).is_err());
    }

    #[test]
    fn safe_join_stays_inside_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "ok").unwrap();
        assert!(safe_join(root.path(), "ui/index.html").is_some());
        assert!(safe_join(root.path(), "../outside").is_none());
        assert!(safe_join(root.path(), "/etc/passwd").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn safe_join_rejects_symlinks_that_escape_the_root() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(safe_join(root.path(), "link/secret").is_none());
    }

    #[test]
    fn atomic_writes_replace_contents_and_leave_no_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let entries = std::fs::read_dir(root.path()).unwrap().count();
        assert_eq!(entries, 1);
    }

    #[test]
    fn module_id_validation_is_strict_but_allows_dots_and_dashes() {
        for id in ["system", "com.example.weather", "vm-lab", "a_b"] {
            assert!(validate_module_id(id).is_ok(), "{id}");
        }
        for id in [
            "",
            ".",
            "..",
            "-bad",
            "has space",
            "has/slash",
            "has\\slash",
            &"a".repeat(200),
        ] {
            assert!(validate_module_id(id).is_err(), "{id}");
        }
    }

    #[test]
    fn git_helpers_initialize_and_commit_a_repository() {
        if !git_available() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(root.path()));
        std::fs::write(root.path().join("file.txt"), "one").unwrap();
        assert!(init_git_repo(root.path()));
        assert!(is_git_repo(root.path()));
        if git_commit(root.path(), "first commit") {
            let log = std::process::Command::new("git")
                .args([
                    "-C",
                    root.path().to_str().unwrap(),
                    "log",
                    "-1",
                    "--pretty=%s",
                ])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "first commit");
        }
    }
}
