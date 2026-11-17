use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zip::ZipArchive;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

/// Preserve the executable bit when a module ships scripts or prebuilt
/// sidecars. Zip stores Unix modes; other platforms ignore the field.
#[cfg(unix)]
fn with_mode(options: SimpleFileOptions, path: &Path) -> SimpleFileOptions {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(metadata) => options.unix_permissions(metadata.permissions().mode() & 0o777),
        Err(_) => options,
    }
}

#[cfg(not(unix))]
fn with_mode(options: SimpleFileOptions, _path: &Path) -> SimpleFileOptions {
    options
}

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
        let options = deflated();
        add_file(zip, options, "config.toml", &paths.config_file)?;
        add_file(
            zip,
            options,
            "theme.css",
            &paths.config_dir.join("theme.css"),
        )?;
        add_tree(
            zip,
            options,
            "themes",
            &paths.config_dir.join("themes"),
            &[],
            false,
        )?;
        add_file(zip, options, "state.json", &paths.state_file)?;
        add_file(
            zip,
            options,
            "dashboards.json",
            &paths.data_dir.join("dashboards.json"),
        )?;
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
        add_file(
            zip,
            options,
            "module-signing.key",
            &paths.data_dir.join("module-signing.key"),
        )?;
        add_file(
            zip,
            options,
            "link/iroh.key",
            &paths.data_dir.join("link/iroh.key"),
        )?;
        add_file(
            zip,
            options,
            "web/iroh.key",
            &paths.data_dir.join("web/iroh.key"),
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
        let options = deflated();
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
                ".env",
                ".env.*",
                "secrets.*",
                "credentials.*",
                "*.pem",
                "*.key",
                "*.p12",
                "*.pfx",
                "id_rsa",
                "id_ed25519",
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
    let Ok(mut file) = crate::util::open_read_no_follow(path) else {
        return Ok(());
    };
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
        "cookie:",
        "set-cookie:",
        "api_key",
        "apikey",
        "password",
        "secret",
        "token",
        "x-orqella-admin-token",
        "x-orqella-module-token",
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
    zip.start_file(name, with_mode(options, path))?;
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, zip)?;
    Ok(())
}

fn excluded_name(name: &str, exclude: &str) -> bool {
    if let Some(suffix) = exclude.strip_prefix("*.") {
        return name.ends_with(&format!(".{suffix}"));
    }
    if let Some(prefix) = exclude.strip_suffix(".*") {
        return name == prefix || name.starts_with(&format!("{prefix}."));
    }
    name == exclude
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
        if excludes.iter().any(|exclude| excluded_name(&name, exclude)) {
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
            zip.start_file(archive_name, with_mode(options, &path))?;
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
    zip.start_file(name, options.unix_permissions(0o600))?;
    zip.write_all(bytes)?;
    Ok(())
}

/// What a backup archive contains. Computed without touching local state, so
/// the CLI can describe the restore before asking for confirmation.
#[derive(Debug, Default, Clone)]
pub struct BackupSummary {
    pub files: usize,
    pub bytes: u64,
    pub modules: Vec<String>,
    pub module_data_files: usize,
    pub custom_themes: usize,
    pub has_config: bool,
    pub has_theme_override: bool,
    pub has_state: bool,
    pub has_dashboards: bool,
    pub has_audit: bool,
    pub has_signing_key: bool,
}

#[derive(Debug, Default, Clone)]
pub struct RestoreReport {
    pub files: usize,
    pub bytes: u64,
    pub modules: Vec<String>,
    pub module_data_files: usize,
    pub custom_themes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Config,
    ThemeOverride,
    Theme,
    State,
    Dashboards,
    Audit,
    ModuleSigningKey,
    LinkSecret,
    WebSecret,
    Module,
    ModuleData,
}

struct PlannedEntry {
    index: usize,
    kind: EntryKind,
    /// Path relative to the restore staging root. Every component has already
    /// been checked for traversal and platform-specific path tricks.
    relative: PathBuf,
    size: u64,
    mode: Option<u32>,
}

/// Inspect a backup without modifying the installation. A support bundle is
/// rejected: it is redacted and must never be treated as a restore artifact.
pub fn inspect_backup(path: &Path) -> Result<BackupSummary> {
    let label = path.display().to_string();
    let mut archive = open_archive(path)?;
    let planned = plan_restore(&mut archive, &label)?;
    validate_archive_config(&mut archive, &planned, &label)?;
    Ok(summarize(&planned))
}

/// Replace the local configuration, themes, module store, modules, and module
/// data with the contents of `source`.
///
/// The archive is fully extracted and validated in a staging directory before
/// any live file is touched. Directories are swapped by renaming the previous
/// tree aside, so a failed extract or validation leaves the installation
/// unchanged.
pub fn restore_backup(paths: &Paths, source: &Path) -> Result<RestoreReport> {
    let label = source.display().to_string();
    let mut archive = open_archive(source)?;
    let planned = plan_restore(&mut archive, &label)?;
    let summary = summarize(&planned);
    paths.ensure()?;

    let staging_root = paths
        .data_dir
        .join(format!(".restore-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&staging_root)
        .with_context(|| format!("could not create {}", staging_root.display()))?;
    crate::util::restrict_dir(&staging_root)?;
    let staging = StagingDir {
        root: staging_root.clone(),
    };

    for entry in &planned {
        let target = staging_root.join(&entry.relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let mut zip_file = archive
            .by_index(entry.index)
            .with_context(|| format!("could not read {} from {label}", entry.relative.display()))?;
        let mut output = create_private_file(&target)?;
        std::io::copy(&mut zip_file, &mut output)
            .with_context(|| format!("could not extract {}", entry.relative.display()))?;
        output.flush()?;
        output
            .sync_all()
            .with_context(|| format!("could not sync {}", target.display()))?;
        drop(output);
        apply_file_mode(&target, entry.mode)?;
    }

    let config_path = staging_root.join("config.toml");
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("could not read {}", config_path.display()))?;
    validate_config(&config_bytes)
        .with_context(|| format!("{label} is not a valid Orqella backup"))?;
    for name in ["state.json", "dashboards.json"] {
        let path = staging_root.join(name);
        if path.is_file() {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .with_context(|| format!("{label} contains an invalid {name}"))?;
        }
    }

    install_file(&config_path, &paths.config_file, true)?;
    install_file(
        &staging_root.join("theme.css"),
        &paths.config_dir.join("theme.css"),
        false,
    )?;
    install_file(
        &staging_root.join("state.json"),
        &paths.state_file,
        false,
    )?;
    install_file(
        &staging_root.join("dashboards.json"),
        &paths.data_dir.join("dashboards.json"),
        false,
    )?;
    install_file(
        &staging_root.join("audit.jsonl"),
        &paths.data_dir.join("audit.jsonl"),
        false,
    )?;

    // Machine identity keys are only replaced when the archive has them.
    // Older backups and deliberate key rotations should keep the local key.
    install_key(
        &staging_root.join("module-signing.key"),
        &paths.data_dir.join("module-signing.key"),
    )?;
    install_key(
        &staging_root.join("link/iroh.key"),
        &paths.data_dir.join("link/iroh.key"),
    )?;
    install_key(
        &staging_root.join("web/iroh.key"),
        &paths.data_dir.join("web/iroh.key"),
    )?;

    replace_directory(
        existing_directory(&staging_root.join("themes")),
        &paths.config_dir.join("themes"),
    )?;
    replace_directory(
        existing_directory(&staging_root.join("modules")),
        &paths.modules_dir,
    )?;
    replace_directory(
        existing_directory(&staging_root.join("module-data")),
        &paths.data_dir.join("module-data"),
    )?;

    drop(staging);
    paths.ensure()?;
    Ok(RestoreReport {
        files: summary.files,
        bytes: summary.bytes,
        modules: summary.modules,
        module_data_files: summary.module_data_files,
        custom_themes: summary.custom_themes,
    })
}

struct StagingDir {
    root: PathBuf,
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn open_archive(path: &Path) -> Result<ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("could not open backup {}", path.display()))?;
    ZipArchive::new(file).with_context(|| format!("could not read backup zip {}", path.display()))
}

fn plan_restore(
    archive: &mut ZipArchive<std::fs::File>,
    label: &str,
) -> Result<Vec<PlannedEntry>> {
    let mut planned = Vec::new();
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("could not read entry {index} from {label}"))?;
        let name = entry
            .name()
            .with_context(|| format!("{label} contains an entry with an invalid name"))?
            .to_string();
        if entry.is_symlink() {
            bail!("{label} contains a symlink entry {name:?}; refusing to restore it");
        }
        if entry.is_dir() {
            let directory = name.trim_end_matches('/');
            if !directory.is_empty() {
                validated_parts(directory)?;
            }
            continue;
        }
        let (kind, relative) = classify_entry(&name)?;
        if !seen.insert(relative.clone()) {
            bail!("{label} contains duplicate entry {name:?}");
        }
        planned.push(PlannedEntry {
            index,
            kind,
            relative,
            size: entry.size(),
            mode: entry.unix_mode(),
        });
    }
    if !planned.iter().any(|entry| entry.kind == EntryKind::Config) {
        bail!("{label} is not an Orqella backup: it has no config.toml");
    }
    Ok(planned)
}

/// Reject absolute paths, traversal, and names that mean something different
/// on Windows. Archive names are always forward-slash separated.
fn validated_parts(name: &str) -> Result<Vec<&str>> {
    if name.is_empty() {
        bail!("archive contains an empty entry name");
    }
    if name.contains('\\') || name.contains('\0') {
        bail!("archive entry {name:?} is not a safe relative path");
    }
    let parts: Vec<&str> = name.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." || part.contains(':') {
            bail!("archive entry {name:?} is not a safe relative path");
        }
    }
    Ok(parts)
}

fn classify_entry(name: &str) -> Result<(EntryKind, PathBuf)> {
    let parts = validated_parts(name)?;
    let kind = match parts[0] {
        "config.toml" if parts.len() == 1 => EntryKind::Config,
        "theme.css" if parts.len() == 1 => EntryKind::ThemeOverride,
        "themes" if parts.len() == 2 => EntryKind::Theme,
        "state.json" if parts.len() == 1 => EntryKind::State,
        "dashboards.json" if parts.len() == 1 => EntryKind::Dashboards,
        "audit.jsonl" if parts.len() == 1 => EntryKind::Audit,
        "module-signing.key" if parts.len() == 1 => EntryKind::ModuleSigningKey,
        "link" if parts.len() == 2 && parts[1] == "iroh.key" => EntryKind::LinkSecret,
        "web" if parts.len() == 2 && parts[1] == "iroh.key" => EntryKind::WebSecret,
        "modules" if parts.len() >= 2 => EntryKind::Module,
        "module-data" if parts.len() >= 2 => EntryKind::ModuleData,
        "config.redacted.toml" | "doctor.txt" | "logs" => {
            bail!(
                "this archive looks like an Orqella support bundle, not a backup; support bundles cannot be restored"
            )
        }
        other => bail!("this file is not an Orqella backup: unexpected entry {other:?}"),
    };
    Ok((kind, PathBuf::from(name)))
}

fn summarize(planned: &[PlannedEntry]) -> BackupSummary {
    let mut summary = BackupSummary::default();
    let mut modules = BTreeSet::new();
    for entry in planned {
        summary.files += 1;
        summary.bytes = summary.bytes.saturating_add(entry.size);
        match entry.kind {
            EntryKind::Config => summary.has_config = true,
            EntryKind::ThemeOverride => summary.has_theme_override = true,
            EntryKind::Theme => {
                if entry.relative.extension().and_then(|value| value.to_str()) == Some("css") {
                    summary.custom_themes += 1;
                }
            }
            EntryKind::State => summary.has_state = true,
            EntryKind::Dashboards => summary.has_dashboards = true,
            EntryKind::Audit => summary.has_audit = true,
            EntryKind::ModuleSigningKey => summary.has_signing_key = true,
            EntryKind::LinkSecret | EntryKind::WebSecret => {}
            EntryKind::Module => {
                if let Some(id) = module_id(&entry.relative) {
                    modules.insert(id);
                }
            }
            EntryKind::ModuleData => summary.module_data_files += 1,
        }
    }
    summary.modules = modules.into_iter().collect();
    summary
}

fn module_id(relative: &Path) -> Option<String> {
    relative
        .components()
        .nth(1)
        .and_then(|component| component.as_os_str().to_str())
        .map(str::to_string)
}

fn validate_archive_config(
    archive: &mut ZipArchive<std::fs::File>,
    planned: &[PlannedEntry],
    label: &str,
) -> Result<()> {
    let Some(entry) = planned.iter().find(|entry| entry.kind == EntryKind::Config) else {
        bail!("{label} is not an Orqella backup: it has no config.toml");
    };
    let mut file = archive.by_index(entry.index)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    validate_config(&bytes).with_context(|| format!("{label} is not a valid Orqella backup"))
}

fn validate_config(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("config.toml is not UTF-8")?;
    toml::from_str::<Config>(text).context("config.toml is not valid Orqella TOML")?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))
}

/// Keep executable bits but never restore group/world write or setuid bits.
fn apply_file_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = ((mode.unwrap_or(0o600) & 0o777) | 0o600) & 0o755;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("could not set permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn install_file(staged: &Path, target: &Path, required: bool) -> Result<()> {
    if !staged.is_file() {
        if required {
            bail!("backup is missing {}", target.display());
        }
        match std::fs::remove_file(target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not remove {}", target.display()));
            }
        }
        return Ok(());
    }
    let bytes = std::fs::read(staged)
        .with_context(|| format!("could not read {}", staged.display()))?;
    crate::util::write_atomic(target, &bytes)?;
    crate::util::restrict_file(target)?;
    Ok(())
}

fn install_key(staged: &Path, target: &Path) -> Result<()> {
    if !staged.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(staged)
        .with_context(|| format!("could not read {}", staged.display()))?;
    crate::util::write_atomic(target, &bytes)?;
    crate::util::restrict_file(target)?;
    Ok(())
}

fn existing_directory(path: &Path) -> Option<&Path> {
    path.is_dir().then_some(path)
}

fn replace_directory(staged: Option<&Path>, target: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(target) {
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to replace symlinked directory {}",
                target.display()
            );
        }
        if !metadata.is_dir() {
            bail!("{} exists and is not a directory", target.display());
        }
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let previous = target.with_extension(format!(
        "restore-old-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let had_previous = target.exists();
    if had_previous {
        std::fs::rename(target, &previous).with_context(|| {
            format!(
                "could not move the current {} aside to {}",
                target.display(),
                previous.display()
            )
        })?;
    }
    let installed = match staged {
        Some(staged) => std::fs::rename(staged, target),
        None => std::fs::create_dir_all(target),
    };
    if let Err(error) = installed {
        if had_previous {
            let _ = std::fs::rename(&previous, target);
        }
        return Err(error).with_context(|| format!("could not install {}", target.display()));
    }
    if had_previous {
        if let Err(error) = std::fs::remove_dir_all(&previous) {
            tracing::warn!(
                %error,
                path = %previous.display(),
                "could not remove the previous directory after restore"
            );
        }
    }
    crate::util::restrict_dir(target)?;
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
        std::fs::write(module.join(".env"), "TOKEN=do-not-ship").unwrap();
        std::fs::write(module.join(".env.local"), "TOKEN=do-not-ship").unwrap();
        std::fs::write(module.join("server.pem"), "do not ship").unwrap();
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
        assert!(archive.by_name("modules/demo/.env").is_err());
        assert!(archive.by_name("modules/demo/.env.local").is_err());
        assert!(archive.by_name("modules/demo/server.pem").is_err());
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
        std::fs::write(paths.config_dir.join("theme.css"), ":root {}").unwrap();
        std::fs::create_dir_all(paths.config_dir.join("themes")).unwrap();
        std::fs::write(paths.config_dir.join("themes/custom-demo.css"), ":root {}").unwrap();
        std::fs::write(paths.config_dir.join("themes/custom-demo.json"), "{}").unwrap();
        std::fs::write(&paths.state_file, "{}").unwrap();
        std::fs::write(paths.data_dir.join("dashboards.json"), "{}").unwrap();
        std::fs::write(paths.data_dir.join("audit.jsonl"), "{}\n").unwrap();
        std::fs::write(paths.data_dir.join("module-signing.key"), "signing").unwrap();
        std::fs::create_dir_all(paths.data_dir.join("link")).unwrap();
        std::fs::write(paths.data_dir.join("link/iroh.key"), "link").unwrap();
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
            "theme.css",
            "themes/custom-demo.css",
            "themes/custom-demo.json",
            "state.json",
            "dashboards.json",
            "audit.jsonl",
            "module-signing.key",
            "link/iroh.key",
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
    fn backup_round_trips_config_themes_modules_and_data() {
        let source_root = tempfile::tempdir().unwrap();
        let source = paths(source_root.path());
        source.ensure().unwrap();
        std::fs::write(&source.config_file, "theme = \"signal\"\n").unwrap();
        std::fs::write(source.config_dir.join("theme.css"), "/* override */").unwrap();
        std::fs::create_dir_all(source.config_dir.join("themes")).unwrap();
        std::fs::write(
            source.config_dir.join("themes/custom-demo.css"),
            ":root { --bg: #000; }",
        )
        .unwrap();
        std::fs::write(
            source.config_dir.join("themes/custom-demo.json"),
            "{\"name\":\"Demo\"}",
        )
        .unwrap();
        std::fs::write(&source.state_file, "{\"modules\":{}}").unwrap();
        std::fs::write(
            source.data_dir.join("dashboards.json"),
            "{\"version\":1}",
        )
        .unwrap();
        std::fs::write(source.data_dir.join("audit.jsonl"), "{\"event\":1}\n").unwrap();
        std::fs::write(
            source.data_dir.join("module-signing.key"),
            "signing-secret",
        )
        .unwrap();
        std::fs::create_dir_all(source.data_dir.join("link")).unwrap();
        std::fs::write(source.data_dir.join("link/iroh.key"), "link-secret").unwrap();
        std::fs::create_dir_all(source.modules_dir.join("demo/ui")).unwrap();
        std::fs::write(
            source.modules_dir.join("demo/ui/index.html"),
            "<h1>demo</h1>",
        )
        .unwrap();
        std::fs::write(source.modules_dir.join("demo/run.sh"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                source.modules_dir.join("demo/run.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::create_dir_all(source.data_dir.join("module-data")).unwrap();
        std::fs::write(
            source.data_dir.join("module-data/demo.json"),
            "{\"saved\":true}",
        )
        .unwrap();

        let destination = source_root.path().join("backup.zip");
        create_backup(&source, &destination).unwrap();
        let summary = inspect_backup(&destination).unwrap();
        assert_eq!(summary.modules, vec!["demo".to_string()]);
        assert_eq!(summary.custom_themes, 1);
        assert!(summary.has_config);
        assert!(summary.has_signing_key);

        let target_root = tempfile::tempdir().unwrap();
        let target = paths(target_root.path());
        target.ensure().unwrap();
        std::fs::write(&target.config_file, "theme = \"daybreak\"\n").unwrap();
        std::fs::write(target.config_dir.join("theme.css"), "/* old */").unwrap();
        std::fs::create_dir_all(target.config_dir.join("themes")).unwrap();
        std::fs::write(target.config_dir.join("themes/old.css"), "old").unwrap();
        std::fs::write(&target.state_file, "{\"old\":true}").unwrap();
        std::fs::write(target.data_dir.join("dashboards.json"), "{\"old\":true}").unwrap();
        std::fs::write(target.data_dir.join("audit.jsonl"), "old\n").unwrap();
        std::fs::create_dir_all(target.modules_dir.join("old")).unwrap();
        std::fs::write(target.modules_dir.join("old/file"), "old").unwrap();
        std::fs::create_dir_all(target.data_dir.join("module-data")).unwrap();
        std::fs::write(target.data_dir.join("module-data/old.json"), "{}").unwrap();

        let report = restore_backup(&target, &destination).unwrap();
        assert_eq!(report.modules, vec!["demo".to_string()]);
        assert_eq!(
            std::fs::read_to_string(&target.config_file).unwrap(),
            "theme = \"signal\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.config_dir.join("theme.css")).unwrap(),
            "/* override */"
        );
        assert!(target.config_dir.join("themes/custom-demo.css").is_file());
        assert!(target.config_dir.join("themes/custom-demo.json").is_file());
        assert!(!target.config_dir.join("themes/old.css").exists());
        assert!(target.modules_dir.join("demo/ui/index.html").is_file());
        assert!(!target.modules_dir.join("old").exists());
        assert!(target.data_dir.join("module-data/demo.json").is_file());
        assert!(!target.data_dir.join("module-data/old.json").exists());
        assert_eq!(
            std::fs::read_to_string(&target.state_file).unwrap(),
            "{\"modules\":{}}"
        );
        assert_eq!(
            std::fs::read_to_string(target.data_dir.join("dashboards.json")).unwrap(),
            "{\"version\":1}"
        );
        assert_eq!(
            std::fs::read_to_string(target.data_dir.join("audit.jsonl")).unwrap(),
            "{\"event\":1}\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.data_dir.join("module-signing.key")).unwrap(),
            "signing-secret"
        );
        assert_eq!(
            std::fs::read_to_string(target.data_dir.join("link/iroh.key")).unwrap(),
            "link-secret"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.modules_dir.join("demo/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "executable bit was not restored");
        }
    }

    #[test]
    fn restore_rejects_path_traversal_without_touching_local_state() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(&paths.config_file, "theme = \"daybreak\"\n").unwrap();
        let archive_path = root.path().join("evil.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("config.toml", options).unwrap();
        zip.write_all(b"theme = \"signal\"\n").unwrap();
        zip.start_file("../escape.txt", options).unwrap();
        zip.write_all(b"escaped").unwrap();
        zip.finish().unwrap();

        let error = restore_backup(&paths, &archive_path).unwrap_err();
        assert!(format!("{error:#}").contains("not a safe relative path"));
        assert!(!root.path().join("escape.txt").exists());
        assert_eq!(
            std::fs::read_to_string(&paths.config_file).unwrap(),
            "theme = \"daybreak\"\n"
        );
    }

    #[test]
    fn restore_rejects_symlinks_and_support_bundles() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(&paths.config_file, "theme = \"daybreak\"\n").unwrap();
        let archive_path = root.path().join("symlink.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("config.toml", options).unwrap();
        zip.write_all(b"theme = \"signal\"\n").unwrap();
        zip.add_symlink("modules/demo/link", "/etc/passwd", options)
            .unwrap();
        zip.finish().unwrap();
        let error = restore_backup(&paths, &archive_path).unwrap_err();
        assert!(format!("{error:#}").contains("symlink"));

        let support = root.path().join("support.zip");
        create_support_bundle(&paths, &Config::default(), &support).unwrap();
        let error = restore_backup(&paths, &support).unwrap_err();
        assert!(format!("{error:#}").contains("support bundle"));
    }

    #[test]
    fn restore_validates_config_before_replacing_anything() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(&paths.config_file, "theme = \"daybreak\"\n").unwrap();
        let archive_path = root.path().join("bad.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("config.toml", options).unwrap();
        zip.write_all(b"this is not toml ==").unwrap();
        zip.finish().unwrap();
        let error = restore_backup(&paths, &archive_path).unwrap_err();
        assert!(format!("{error:#}").contains("valid Orqella backup"));
        assert_eq!(
            std::fs::read_to_string(&paths.config_file).unwrap(),
            "theme = \"daybreak\"\n"
        );
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
