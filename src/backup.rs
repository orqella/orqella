use crate::config::{Config, Paths};
use anyhow::{Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

fn create_archive(
    destination: &Path,
    build: impl FnOnce(&mut ZipWriter<std::fs::File>) -> Result<()>,
) -> Result<PathBuf> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension(format!("tmp.{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&temporary)
        .with_context(|| format!("could not create {}", temporary.display()))?;
    let mut zip = ZipWriter::new(file);
    let write_result = build(&mut zip);
    let file = match write_result {
        Ok(()) => zip.finish(),
        Err(error) => {
            drop(zip);
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
    };
    let file = file.context("could not finish archive")?;
    file.sync_all()
        .with_context(|| format!("could not sync {}", temporary.display()))?;
    drop(file);
    std::fs::rename(&temporary, destination)
        .with_context(|| format!("could not replace {}", destination.display()))?;
    Ok(destination.to_path_buf())
}

pub fn create_backup(paths: &Paths, destination: &Path) -> Result<PathBuf> {
    create_archive(destination, |zip| {
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        add_file(zip, options, "config.toml", &paths.config_file)?;
        add_file(zip, options, "state.json", &paths.state_file)?;
        add_tree(
            zip,
            options,
            "modules",
            &paths.modules_dir,
            &["sidecar.log"],
            false,
        )?;
        add_tree(
            zip,
            options,
            "module-data",
            &paths.data_dir.join("module-data"),
            &[],
            false,
        )?;
        add_file(
            zip,
            options,
            "audit.jsonl",
            &paths.data_dir.join("audit.jsonl"),
        )?;
        Ok(())
    })
}

pub fn create_support_bundle(
    paths: &Paths,
    config: &Config,
    destination: &Path,
) -> Result<PathBuf> {
    let mut redacted = config.clone();
    redacted.peer_token = Some("[redacted]".into());
    for peer in &mut redacted.peers {
        peer.token = "[redacted]".into();
    }
    redacted.link.token = Some("[redacted]".into());
    for value in redacted.agent.env.values_mut() {
        *value = "[redacted]".into();
    }
    if !redacted.agent.args.is_empty() {
        redacted.agent.args = vec!["[redacted]".into()];
    }
    for mount in &mut redacted.proxy {
        mount.base_url = crate::config::redact_url_userinfo(&mount.base_url);
    }
    let config_text = toml::to_string_pretty(&redacted)?;
    let mut doctor = String::new();
    doctor.push_str(&format!("orqella {}\n", env!("CARGO_PKG_VERSION")));
    doctor.push_str(&format!("config: {}\n", paths.config_file.display()));
    doctor.push_str(&format!("data: {}\n", paths.data_dir.display()));
    doctor.push_str(&format!("dashboard: {}\n", config.server.dashboard));
    doctor.push_str(&format!("origin: {}\n", config.origin()));
    doctor.push_str(&format!(
        "git: {}\n",
        if crate::util::git_available() {
            "available"
        } else {
            "missing"
        }
    ));
    doctor.push_str(&format!("proxy mounts: {}\n", config.proxy.len()));
    doctor.push_str(&format!("link peers: {}\n", config.peers.len()));
    create_archive(destination, |zip| {
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        write_bytes(zip, options, "config.redacted.toml", config_text.as_bytes())?;
        add_tree(
            zip,
            options,
            "modules",
            &paths.modules_dir,
            &[
                "sidecar.log",
                "*.log",
                "target",
                "node_modules",
                ".venv",
                ".git",
                ".config",
                ".cache",
                ".local",
            ],
            true,
        )?;
        write_bytes(zip, options, "doctor.txt", doctor.as_bytes())?;
        add_log_tail(
            zip,
            options,
            "logs/update.log",
            &paths.data_dir.join("update.log"),
        )?;
        add_log_tree(zip, options, "logs", &paths.logs_dir)?;
        for module in module_ids(&paths.modules_dir) {
            add_log_tail(
                zip,
                options,
                &format!("logs/modules/{module}/sidecar.log"),
                &paths.modules_dir.join(&module).join("sidecar.log"),
            )?;
        }
        Ok(())
    })
}

/// Maximum bytes copied from any single log. Support bundles are for triage,
/// not archival, and the last few pages are what matters.
const LOG_TAIL_BYTES: u64 = 32 * 1024;

fn module_ids(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

fn add_log_tree<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    prefix: &str,
    root: &Path,
) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if !path.is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("log")
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        add_log_tail(zip, options, &format!("{prefix}/{name}"), &path)?;
    }
    Ok(())
}

fn add_log_tail<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    path: &Path,
) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length > LOG_TAIL_BYTES {
        file.seek(SeekFrom::End(-(LOG_TAIL_BYTES as i64)))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let redacted = redact_log(&text);
    if !redacted.trim().is_empty() {
        write_bytes(zip, options, name, redacted.as_bytes())?;
    }
    Ok(())
}

/// Logs can contain module tokens, admin tokens, or Authorization headers.
/// Replace any line that looks like it carries a secret. This is deliberately
/// conservative: losing a line is better than leaking a credential.
fn redact_log(text: &str) -> String {
    const MARKERS: &[&str] = &[
        "authorization:",
        "bearer ",
        "password",
        "secret",
        "token",
        "x-orqella-admin-token",
        "x-orqella-peer-token",
    ];
    text.lines()
        .map(|line| {
            let lowered = line.to_ascii_lowercase();
            if MARKERS.iter().any(|marker| lowered.contains(marker)) {
                "[redacted log line]\n".to_string()
            } else {
                format!("{line}\n")
            }
        })
        .collect()
}

fn add_file<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    path: &Path,
) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    zip.start_file(name, options)?;
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, zip)?;
    Ok(())
}

fn add_tree<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    prefix: &str,
    root: &Path,
    excludes: &[&str],
    skip_module_data: bool,
) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    add_tree_inner(zip, options, prefix, root, root, excludes, skip_module_data)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_tree_inner<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    prefix: &str,
    root: &Path,
    dir: &Path,
    excludes: &[&str],
    skip_module_data: bool,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if excludes.iter().any(|exclude| {
            if let Some(suffix) = exclude.strip_prefix("*.") {
                name.ends_with(&format!(".{suffix}"))
            } else {
                name == *exclude
            }
        }) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        if skip_module_data
            && prefix == "modules"
            && relative.components().count() == 2
            && name.eq_ignore_ascii_case("data")
        {
            continue;
        }
        let archive_name = format!("{prefix}/{}", relative.to_string_lossy().replace('\\', "/"));
        if entry.file_type()?.is_dir() {
            add_tree_inner(
                zip,
                options,
                prefix,
                root,
                &path,
                excludes,
                skip_module_data,
            )?;
        } else if entry.file_type()?.is_file() {
            zip.start_file(archive_name, options)?;
            let mut file = std::fs::File::open(path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

fn write_bytes<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    zip.start_file(name, options)?;
    zip.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use zip::ZipArchive;

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
    fn support_bundle_redacts_secrets_and_omits_runtime_data() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let module = paths.modules_dir.join("demo");
        std::fs::create_dir_all(module.join("ui")).unwrap();
        std::fs::create_dir_all(module.join("data")).unwrap();
        std::fs::create_dir_all(module.join(".config")).unwrap();
        std::fs::write(module.join("ui/index.html"), "page").unwrap();
        std::fs::write(module.join("data/secret.json"), "do not ship").unwrap();
        std::fs::write(module.join(".config/secret"), "do not ship").unwrap();
        std::fs::write(paths.state_file.clone(), "{}").unwrap();
        std::fs::write(paths.data_dir.join("audit.jsonl"), "secret prompt\n").unwrap();
        std::fs::write(
            paths.data_dir.join("update.log"),
            "started\nmodule token=abc123\nfinished\n",
        )
        .unwrap();
        std::fs::write(module.join("sidecar.log"), "ready\nAuthorization: secret\n").unwrap();
        let mut config = Config::default();
        config.link.token = Some("link-secret".into());
        config.agent.args = vec!["--token".into(), "agent-arg-secret".into()];
        config
            .agent
            .env
            .insert("API_TOKEN".into(), "agent-secret".into());
        config.proxy.push(crate::config::ProxyConfig {
            id: "demo".into(),
            title: "Demo".into(),
            base_url: "http://user:proxy-secret@127.0.0.1:8080".into(),
            host: None,
            websocket: true,
            frame: false,
        });
        let destination = root.path().join("support.zip");
        create_support_bundle(&paths, &config, &destination).unwrap();

        let mut archive = ZipArchive::new(std::fs::File::open(destination).unwrap()).unwrap();
        assert!(archive.by_name("state.json").is_err());
        assert!(archive.by_name("audit.jsonl").is_err());
        assert!(archive.by_name("modules/demo/data/secret.json").is_err());
        assert!(archive.by_name("modules/demo/.config/secret").is_err());
        let mut config_text = String::new();
        archive
            .by_name("config.redacted.toml")
            .unwrap()
            .read_to_string(&mut config_text)
            .unwrap();
        assert!(!config_text.contains("link-secret"));
        assert!(!config_text.contains("agent-secret"));
        assert!(!config_text.contains("agent-arg-secret"));
        assert!(!config_text.contains("proxy-secret"));
        assert!(config_text.contains("[redacted]"));
        let mut update_log = String::new();
        archive
            .by_name("logs/update.log")
            .unwrap()
            .read_to_string(&mut update_log)
            .unwrap();
        assert!(update_log.contains("started"));
        assert!(!update_log.contains("abc123"));
        let mut sidecar_log = String::new();
        archive
            .by_name("logs/modules/demo/sidecar.log")
            .unwrap()
            .read_to_string(&mut sidecar_log)
            .unwrap();
        assert!(sidecar_log.contains("ready"));
        assert!(!sidecar_log.contains("Authorization: secret"));
    }

    #[test]
    fn backup_contains_config_state_modules_and_runtime_data_but_not_logs() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(&paths.config_file, "theme = \"signal\"\n").unwrap();
        std::fs::write(&paths.state_file, "{}").unwrap();
        std::fs::write(paths.data_dir.join("audit.jsonl"), "{}\n").unwrap();
        let module = paths.modules_dir.join("demo");
        std::fs::create_dir_all(module.join("ui")).unwrap();
        std::fs::create_dir_all(module.join("data")).unwrap();
        std::fs::write(module.join("ui/index.html"), "page").unwrap();
        std::fs::write(module.join("data/private.json"), "runtime").unwrap();
        std::fs::write(module.join("sidecar.log"), "log").unwrap();
        std::fs::create_dir_all(paths.data_dir.join("module-data/demo")).unwrap();
        std::fs::write(paths.data_dir.join("module-data/demo/state.json"), "{}").unwrap();

        let destination = root.path().join("backup.zip");
        create_backup(&paths, &destination).unwrap();
        let mut archive = ZipArchive::new(std::fs::File::open(destination).unwrap()).unwrap();
        let mut names = Vec::new();
        for index in 0..archive.len() {
            names.push(archive.by_index(index).unwrap().name().unwrap().to_string());
        }
        for expected in [
            "config.toml",
            "state.json",
            "audit.jsonl",
            "modules/demo/ui/index.html",
            "modules/demo/data/private.json",
            "module-data/demo/state.json",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected}: {names:?}"
            );
        }
        assert!(!names.iter().any(|name| name.ends_with("sidecar.log")));
    }

    #[test]
    fn failed_archive_build_does_not_leave_a_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("backup.zip");
        let error = create_archive(&destination, |_zip| anyhow::bail!("boom")).unwrap_err();
        assert!(format!("{error:#}").contains("boom"));
        assert!(!destination.exists());
        let leftovers = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(leftovers, 0);
    }
}
