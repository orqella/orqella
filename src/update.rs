use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use url::Url;

pub const UPDATE_MANIFEST: &str = "ORQELLA_UPDATE.json";
pub const UPDATE_SIGNATURE: &str = "ORQELLA_UPDATE.json.minisig";
pub const CHECKSUMS: &str = "SHA256SUMS.txt";
const UPDATE_MANIFEST_SCHEMA: u32 = 1;
const UPDATE_STATE_FILE: &str = "update-state.json";
const GITHUB_API: &str = "https://api.github.com";
const DEFAULT_REPOSITORY: &str = "orqella/orqella";
const MAX_RELEASES_PAGES: usize = 3;
const MAX_METADATA_BYTES: u64 = 256 * 1024;
const MAX_UPDATE_ASSET_BYTES: u64 = 512 * 1024 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub schema: u32,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppliedUpdate {
    #[serde(default)]
    version: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    asset: String,
    #[serde(default)]
    applied_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone)]
struct ReleaseCandidate {
    version: Version,
    tag: String,
    published_at: String,
    notes: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone)]
struct ResolvedRelease {
    release: ReleaseCandidate,
    asset: GithubAsset,
    checksum: GithubAsset,
    signed: bool,
    platform: &'static str,
}

enum RestartMode {
    Service,
    Spawned,
    Manual,
}

fn github_client() -> Result<reqwest::blocking::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_str(&format!("orqella/{}", env!("CARGO_PKG_VERSION")))?,
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        reqwest::header::HeaderValue::from_static("2022-11-28"),
    );
    if let Ok(token) = std::env::var("ORQELLA_GITHUB_TOKEN")
        && !token.trim().is_empty()
    {
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.trim()))?,
        );
    }
    reqwest::blocking::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .build()
        .context("could not create the GitHub API client")
}

fn download_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("orqella/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .context("could not create the update download client")
}

fn normalize_repository(value: &str) -> Result<String> {
    let value = value.trim();
    let value = if value.is_empty() {
        DEFAULT_REPOSITORY
    } else {
        value
    };
    let value = value.trim_end_matches('/');
    let value = value.strip_suffix(".git").unwrap_or(value);
    let value = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("http://github.com/"))
        .unwrap_or(value);
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let repository = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || !valid_repository_component(owner)
        || !valid_repository_component(repository)
    {
        bail!("[update] repository must be owner/repo or a GitHub repository URL");
    }
    Ok(format!("{owner}/{repository}"))
}

fn valid_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn fetch_releases(
    client: &reqwest::blocking::Client,
    repository: &str,
) -> Result<Vec<GithubRelease>> {
    let mut releases = Vec::new();
    for page in 1..=MAX_RELEASES_PAGES {
        let url = format!("{GITHUB_API}/repos/{repository}/releases?per_page=100&page={page}");
        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("could not reach the GitHub Releases API for {repository}"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            bail!(
                "GitHub Releases API returned {status}: {}",
                body_tail(&body)
            );
        }
        let batch: Vec<GithubRelease> = serde_json::from_str(&body)
            .context("GitHub Releases API returned an invalid release list")?;
        if batch.is_empty() {
            break;
        }
        releases.extend(batch);
    }
    Ok(releases)
}

fn tag_version(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

fn latest_release(releases: Vec<GithubRelease>) -> Result<ReleaseCandidate> {
    let mut best: Option<ReleaseCandidate> = None;
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let Some(version) = tag_version(&release.tag_name) else {
            continue;
        };
        if !version.pre.is_empty() {
            continue;
        }
        let candidate = ReleaseCandidate {
            version: version.clone(),
            tag: release.tag_name,
            published_at: release.published_at.unwrap_or_default(),
            notes: truncate(release.body.unwrap_or_default(), 4_000),
            assets: release.assets,
        };
        if best
            .as_ref()
            .is_none_or(|current| candidate.version > current.version)
        {
            best = Some(candidate);
        }
    }
    best.context("no published daemon release was found (expected a v<version> tag)")
}

fn platform() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(("linux-x86_64", ".tar.gz")),
        ("macos", "x86_64") => Ok(("macos-x86_64", ".tar.gz")),
        ("macos", "aarch64") => Ok(("macos-arm64", ".tar.gz")),
        ("windows", "x86_64") => Ok(("windows-x86_64", ".zip")),
        (os, arch) => bail!("no daemon release asset is published for {os}/{arch}"),
    }
}

fn find_asset(assets: &[GithubAsset], name: &str) -> Option<GithubAsset> {
    assets.iter().find(|asset| asset.name == name).cloned()
}

fn platform_asset(assets: &[GithubAsset], platform: &str, extension: &str) -> Option<GithubAsset> {
    let suffix = format!("-{platform}{extension}");
    assets
        .iter()
        .find(|asset| {
            is_safe_asset_name(&asset.name)
                && asset.name.starts_with("orqella-")
                && asset.name.ends_with(&suffix)
        })
        .cloned()
}

fn is_safe_asset_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn checksum_asset(assets: &[GithubAsset], archive: &GithubAsset) -> Option<GithubAsset> {
    find_asset(assets, CHECKSUMS)
        .or_else(|| find_asset(assets, &format!("{}.sha256", archive.name)))
}

fn resolve_release(
    config: &Config,
    client: &reqwest::blocking::Client,
    release: ReleaseCandidate,
) -> Result<ResolvedRelease> {
    let (platform, extension) = platform()?;
    let asset = platform_asset(&release.assets, platform, extension).with_context(|| {
        format!(
            "release {} has no {platform} archive ({extension})",
            release.tag
        )
    })?;
    let checksum = checksum_asset(&release.assets, &asset).with_context(|| {
        format!(
            "release {} has no {CHECKSUMS} or {}.sha256 asset",
            release.tag, asset.name
        )
    })?;
    let mut signed = false;
    if !config.update.public_key.trim().is_empty()
        || option_env!("ORQELLA_UPDATE_PUBLIC_KEY").is_some_and(|key| !key.trim().is_empty())
    {
        let manifest_asset = find_asset(&release.assets, UPDATE_MANIFEST).with_context(|| {
            format!(
                "release {} is not signed because it has no {UPDATE_MANIFEST}",
                release.tag
            )
        })?;
        let signature_asset = find_asset(&release.assets, UPDATE_SIGNATURE).with_context(|| {
            format!(
                "release {} is not signed because it has no {UPDATE_SIGNATURE}",
                release.tag
            )
        })?;
        let manifest = download_bytes(client, &manifest_asset, MAX_METADATA_BYTES)?;
        let signature = download_bytes(client, &signature_asset, MAX_METADATA_BYTES)?;
        verify_release_manifest(config, &release, &manifest, &signature)?;
        signed = true;
    }
    Ok(ResolvedRelease {
        release,
        asset,
        checksum,
        signed,
        platform,
    })
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

fn verify_release_manifest(
    config: &Config,
    release: &ReleaseCandidate,
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<()> {
    if manifest_bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!("update manifest is too large");
    }
    let manifest: UpdateManifest =
        serde_json::from_slice(manifest_bytes).context("update manifest is not valid JSON")?;
    if manifest.schema != UPDATE_MANIFEST_SCHEMA {
        bail!(
            "unsupported update manifest schema {}; expected {UPDATE_MANIFEST_SCHEMA}",
            manifest.schema
        );
    }
    let version = Version::parse(&manifest.version)
        .with_context(|| format!("invalid release version {:?}", manifest.version))?;
    if version != release.version {
        bail!(
            "update manifest version {version} does not match release {}",
            release.tag
        );
    }
    let signature_text =
        std::str::from_utf8(signature_bytes).context("update signature is not UTF-8")?;
    let signature =
        Signature::decode(signature_text).context("update manifest signature is malformed")?;
    let trusted = signature.trusted_comment();
    if !trusted.contains(&format!("version:{}", manifest.version)) {
        bail!("update manifest signature does not cover its version");
    }
    update_public_key(config)?
        .verify(manifest_bytes, &signature, false)
        .context("update manifest signature did not verify")?;
    Ok(())
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn applied_update(paths: &Paths) -> Option<AppliedUpdate> {
    let raw = std::fs::read_to_string(paths.data_dir.join(UPDATE_STATE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn record_applied(paths: &Paths, update: &ResolvedRelease) -> Result<()> {
    let value = AppliedUpdate {
        version: update.release.version.to_string(),
        tag: update.release.tag.clone(),
        asset: update.asset.name.clone(),
        applied_at: crate::util::now_iso(),
    };
    let raw = serde_json::to_vec_pretty(&value)?;
    let path = paths.data_dir.join(UPDATE_STATE_FILE);
    crate::util::write_atomic(&path, &raw)?;
    let _ = crate::util::restrict_file(&path);
    Ok(())
}

fn enforce_forward_update(paths: &Paths, version: &Version) -> Result<()> {
    let current = current_version();
    if version <= &current {
        bail!("release {version} is not newer than the running version {current}");
    }
    if let Some(applied) = applied_update(paths)
        && let Ok(applied_version) = Version::parse(&applied.version)
        && version <= &applied_version
    {
        bail!(
            "release {version} is not newer than the last applied update {applied_version} (rollback refused)"
        );
    }
    Ok(())
}

pub fn check(config: &Config, paths: &Paths) -> Result<Value> {
    let repository = normalize_repository(&config.update.repository)?;
    let client = github_client()?;
    let release = latest_release(fetch_releases(&client, &repository)?)?;
    let current = current_version();
    let mut value = json!({
        "ok": true,
        "repository": repository,
        "current": current.to_string(),
        "latest": release.version.to_string(),
        "latest_tag": release.tag,
        "published_at": release.published_at,
        "notes": release.notes,
        "available": false,
        "signed": false,
    });
    if release.version <= current {
        return Ok(value);
    }
    enforce_forward_update(paths, &release.version)?;
    let resolved = resolve_release(config, &client, release)?;
    value["available"] = json!(true);
    value["asset"] = json!(resolved.asset.name);
    value["asset_size"] = json!(resolved.asset.size);
    value["platform"] = json!(resolved.platform);
    value["signed"] = json!(resolved.signed);
    value["release_url"] = json!(format!(
        "https://github.com/{}/releases/tag/{}",
        normalize_repository(&config.update.repository)?,
        resolved.release.tag
    ));
    Ok(value)
}

fn validate_asset_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("update asset has an invalid download URL")?;
    if url.scheme() != "https" {
        bail!("refusing a non-HTTPS update asset URL");
    }
    Ok(url)
}

fn download_asset(
    client: &reqwest::blocking::Client,
    asset: &GithubAsset,
    destination: &Path,
    max_bytes: u64,
) -> Result<()> {
    validate_asset_url(&asset.browser_download_url)?;
    if asset.size > max_bytes {
        bail!(
            "update asset {} is larger than the {} byte limit",
            asset.name,
            max_bytes
        );
    }
    let response = client
        .get(&asset.browser_download_url)
        .send()
        .with_context(|| format!("could not download {}", asset.name))?;
    let final_url = response.url().clone();
    if final_url.scheme() != "https" {
        bail!("refusing a non-HTTPS update download from {final_url}");
    }
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!(
            "downloading {} returned {status}: {}",
            asset.name,
            body_tail(&body)
        );
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(destination)
        .with_context(|| format!("could not create {}", destination.display()))?;
    let _ = crate::util::restrict_file(destination);
    let copied = std::io::copy(&mut response.take(max_bytes.saturating_add(1)), &mut file)
        .with_context(|| format!("could not save {}", asset.name))?;
    if copied > max_bytes {
        let _ = std::fs::remove_file(destination);
        bail!("update asset {} exceeded the size limit", asset.name);
    }
    file.sync_all()?;
    Ok(())
}

fn download_bytes(
    client: &reqwest::blocking::Client,
    asset: &GithubAsset,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    validate_asset_url(&asset.browser_download_url)?;
    let response = client
        .get(&asset.browser_download_url)
        .send()
        .with_context(|| format!("could not download {}", asset.name))?;
    let final_url = response.url().clone();
    if final_url.scheme() != "https" {
        bail!("refusing a non-HTTPS update download from {final_url}");
    }
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!(
            "downloading {} returned {status}: {}",
            asset.name,
            body_tail(&body)
        );
    }
    let mut bytes = Vec::new();
    response
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", asset.name))?;
    if bytes.len() as u64 > max_bytes {
        bail!("update file {} exceeded the size limit", asset.name);
    }
    Ok(bytes)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
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

fn parse_checksum(checksums: &str, asset_name: &str) -> Result<String> {
    for line in checksums.lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let name = parts.next().unwrap_or(asset_name).trim_start_matches('*');
        if name != asset_name {
            continue;
        }
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("checksum for {asset_name} is not a SHA-256 digest");
        }
        return Ok(hash.to_ascii_lowercase());
    }
    bail!("checksum file does not contain {asset_name}")
}

fn is_binary_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "orqella" || name == "orqella.exe")
}

fn extract_binary(archive: &Path, asset_name: &str, destination: &Path) -> Result<()> {
    if asset_name.ends_with(".zip") {
        extract_zip_binary(archive, destination)
    } else {
        extract_tar_binary(archive, destination)
    }
}

fn extract_tar_binary(archive: &Path, destination: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("could not read update archive")? {
        let mut entry = entry.context("could not read an update archive entry")?;
        let path = entry.path().context("update archive entry has no path")?;
        if !is_binary_name(&path) {
            continue;
        }
        if entry.header().size().unwrap_or(0) > MAX_UPDATE_ASSET_BYTES {
            bail!("update binary is larger than the size limit");
        }
        let mut output = std::fs::File::create(destination)
            .with_context(|| format!("could not create {}", destination.display()))?;
        std::io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        return Ok(());
    }
    bail!("update archive does not contain an orqella binary")
}

fn extract_zip_binary(archive: &Path, destination: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("could not read update archive")?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("could not read an update archive entry")?;
        let name = entry.name().context("update archive entry has no name")?;
        if !is_binary_name(Path::new(name.as_ref())) {
            continue;
        }
        if entry.size() > MAX_UPDATE_ASSET_BYTES {
            bail!("update binary is larger than the size limit");
        }
        let mut output = std::fs::File::create(destination)
            .with_context(|| format!("could not create {}", destination.display()))?;
        std::io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        return Ok(());
    }
    bail!("update archive does not contain an orqella binary")
}

fn current_exe_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running orqella binary")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn install_binary(new_binary: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .with_context(|| format!("{} has no parent directory", target.display()))?;
    let temporary = parent.join(format!(".orqella-update-{}", uuid::Uuid::new_v4()));
    std::fs::copy(new_binary, &temporary)
        .with_context(|| format!("could not stage {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Ok(file) = std::fs::File::open(&temporary) {
        let _ = file.sync_all();
    }

    #[cfg(windows)]
    {
        let backup = target.with_file_name(format!(
            "{}.old-{}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("orqella.exe"),
            uuid::Uuid::new_v4()
        ));
        std::fs::rename(target, &backup).with_context(|| {
            format!("could not move the running binary to {}", backup.display())
        })?;
        if let Err(error) = std::fs::rename(&temporary, target) {
            let _ = std::fs::rename(&backup, target);
            return Err(error).with_context(|| format!("could not install {}", target.display()));
        }
        let _ = std::fs::remove_file(&backup);
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&temporary, target)
            .with_context(|| format!("could not install {}", target.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn restart_service() -> Result<bool> {
    let Some(config_dir) = dirs::config_dir() else {
        return Ok(false);
    };
    let unit = config_dir.join("systemd/user/orqella.service");
    if !unit.is_file() {
        return Ok(false);
    }
    let status = Command::new("systemctl")
        .args(["--user", "restart", "orqella.service"])
        .status()
        .context("could not run systemctl")?;
    if !status.success() {
        bail!("systemctl could not restart orqella.service");
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn restart_service() -> Result<bool> {
    let Some(home) = dirs::home_dir() else {
        return Ok(false);
    };
    let plist = home.join("Library/LaunchAgents/com.orqella.daemon.plist");
    if !plist.is_file() {
        return Ok(false);
    }
    let uid = unsafe { libc::getuid() };
    let service = format!("gui/{uid}/com.orqella.daemon");
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", &service])
        .status()
        .context("could not run launchctl")?;
    if !status.success() {
        bail!("launchctl could not restart com.orqella.daemon");
    }
    Ok(true)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn restart_service() -> Result<bool> {
    Ok(false)
}

fn stop_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn daemon_responds(config: &Config) -> bool {
    let url = format!("{}healthz", config.loopback_origin());
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .is_some_and(|client| {
            client
                .get(&url)
                .send()
                .is_ok_and(|response| response.status().is_success())
        })
}

fn spawn_daemon(paths: &Paths, executable: &Path) -> Result<()> {
    let log_path = paths.logs_dir.join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("could not open {}", log_path.display()))?;
    let _ = crate::util::restrict_file(&log_path);
    let log_err = log.try_clone()?;
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
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
    command
        .spawn()
        .context("could not start the updated daemon")?;
    Ok(())
}

fn restart_daemon(
    config: &Config,
    paths: &Paths,
    executable: &Path,
    parent_pid: Option<u32>,
) -> Result<RestartMode> {
    if restart_service()? {
        return Ok(RestartMode::Service);
    }
    if let Some(pid) = parent_pid {
        stop_process(pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while daemon_responds(config) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        if daemon_responds(config) {
            bail!("the old daemon did not stop; restart it manually and retry");
        }
        spawn_daemon(paths, executable)?;
        return Ok(RestartMode::Spawned);
    }
    if daemon_responds(config) {
        return Ok(RestartMode::Manual);
    }
    spawn_daemon(paths, executable)?;
    Ok(RestartMode::Spawned)
}

fn wait_for_health(config: &Config, expected: &Version) -> Result<()> {
    let url = format!("{}healthz", config.loopback_origin());
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("could not create the health-check client")?;
    let expected = expected.to_string();
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(response) = client.get(&url).send()
            && response.status().is_success()
        {
            let value: Value = response.json().unwrap_or_default();
            let version = value.get("version").and_then(Value::as_str);
            if version.is_none_or(|version| version == expected) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("the updated daemon did not become healthy at {url}")
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
        &status_value("failed", "failed", &format!("{error}\n")),
    );
}

fn status_value(status: &str, phase: &str, log: &str) -> Value {
    json!({
        "status": status,
        "phase": phase,
        "log": log,
        "updated_at": crate::util::now_iso(),
    })
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
    let mut state = status_value("running", "checking", "");
    write_status(paths, &state);

    let result = apply_inner(config, paths, parent_pid, &mut state);
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
    parent_pid: Option<u32>,
    state: &mut Value,
) -> Result<()> {
    let repository = normalize_repository(&config.update.repository)?;
    let client = github_client()?;
    *state = log_line(paths, state, &format!("Checking {repository} releases…"));
    let release = latest_release(fetch_releases(&client, &repository)?)?;
    if release.version <= current_version() {
        *state = log_line(
            paths,
            state,
            &format!("Already running {}.", current_version()),
        );
        return Ok(());
    }
    enforce_forward_update(paths, &release.version)?;
    let resolved = resolve_release(config, &client, release)?;
    *state = log_line(
        paths,
        state,
        &format!(
            "Downloading {} ({})…",
            resolved.release.tag,
            if resolved.signed {
                "signed"
            } else {
                "checksum verified"
            }
        ),
    );

    let updates_dir = paths.data_dir.join("updates");
    std::fs::create_dir_all(&updates_dir)
        .with_context(|| format!("could not create {}", updates_dir.display()))?;
    let staging = tempfile::Builder::new()
        .prefix("release-")
        .tempdir_in(&updates_dir)
        .context("could not create the update staging directory")?;
    let download_client = download_client()?;
    let archive_path = staging.path().join(&resolved.asset.name);
    download_asset(
        &download_client,
        &resolved.asset,
        &archive_path,
        MAX_UPDATE_ASSET_BYTES,
    )?;

    *state = log_line(paths, state, "Verifying the release checksum…");
    let checksums = download_bytes(&download_client, &resolved.checksum, MAX_METADATA_BYTES)?;
    let checksums = std::str::from_utf8(&checksums).context("checksum file is not UTF-8")?;
    let expected = parse_checksum(checksums, &resolved.asset.name)?;
    let actual = sha256_file(&archive_path)?;
    if actual != expected {
        bail!(
            "checksum mismatch for {}: expected {expected}, got {actual}",
            resolved.asset.name
        );
    }
    state["version"] = json!(resolved.release.version.to_string());
    state["tag"] = json!(resolved.release.tag);
    state["asset"] = json!(resolved.asset.name);
    state["signed"] = json!(resolved.signed);

    *state = log_line(paths, state, "Extracting the new binary…");
    let extracted = staging.path().join("orqella-new");
    extract_binary(&archive_path, &resolved.asset.name, &extracted)?;

    *state = log_line(paths, state, "Installing the new binary…");
    let executable = current_exe_path()?;
    install_binary(&extracted, &executable)?;

    *state = log_line(paths, state, "Restarting the daemon…");
    match restart_daemon(config, paths, &executable, parent_pid)? {
        RestartMode::Service => {
            *state = log_line(paths, state, "Restarted the per-user service.");
        }
        RestartMode::Spawned => {
            *state = log_line(paths, state, "Started the updated daemon.");
        }
        RestartMode::Manual => {
            bail!(
                "the new binary is installed, but no managed service was found; restart the running daemon to use it"
            );
        }
    }

    *state = log_line(paths, state, "Waiting for the updated daemon…");
    wait_for_health(config, &resolved.release.version)?;
    record_applied(paths, &resolved)?;
    *state = log_line(
        paths,
        state,
        &format!("Updated to Orqella {}.", resolved.release.version),
    );
    Ok(())
}

pub fn spawn(config: &Config, paths: &Paths, parent_pid: u32) -> Result<()> {
    let _ = normalize_repository(&config.update.repository)?;
    write_status(paths, &status_value("queued", "starting", ""));
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

fn body_tail(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(500)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn truncate(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut value: String = value.chars().take(max_chars).collect();
    value.push('…');
    value
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use base64::Engine;
    use blake2::{Blake2b512, Digest};
    use ed25519_dalek::{Signer, SigningKey};

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

    fn asset(name: &str) -> GithubAsset {
        GithubAsset {
            name: name.into(),
            browser_download_url: format!(
                "https://github.com/orqella/orqella/releases/download/v1/{name}"
            ),
            size: 10,
        }
    }

    fn release(tag: &str, prerelease: bool, draft: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            draft,
            prerelease,
            published_at: Some("2026-01-01T00:00:00Z".into()),
            body: Some("notes".into()),
            assets: vec![
                asset("orqella-9.9.9-linux-x86_64.tar.gz"),
                asset("orqella-9.9.9-windows-x86_64.zip"),
                asset("SHA256SUMS.txt"),
            ],
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
        mark_failed(&paths, "download exploded");
        let value = status(&paths);
        assert_eq!(value["status"], "failed");
        assert_eq!(value["phase"], "failed");
        assert!(value["log"].as_str().unwrap().contains("download exploded"));
        assert!(value["updated_at"].as_str().is_some());
    }

    #[test]
    fn repository_accepts_owner_repo_and_github_urls() {
        assert_eq!(
            normalize_repository("orqella/orqella").unwrap(),
            "orqella/orqella"
        );
        assert_eq!(
            normalize_repository("https://github.com/orqella/orqella.git").unwrap(),
            "orqella/orqella"
        );
        assert!(normalize_repository("not-a-repo").is_err());
        assert!(normalize_repository("owner/repo/extra").is_err());
    }

    #[test]
    fn latest_release_ignores_desktop_prerelease_and_draft_tags() {
        let releases = vec![
            release("v0.4.0", false, false),
            release("desktop-v9.9.9", false, false),
            release("v0.5.0-rc.1", false, false),
            release("v8.0.0", true, false),
            release("v7.0.0", false, true),
            release("v0.3.1", false, false),
        ];
        let latest = latest_release(releases).unwrap();
        assert_eq!(latest.version, Version::new(0, 4, 0));
        assert_eq!(latest.tag, "v0.4.0");
    }

    #[test]
    fn platform_and_checksum_assets_are_selected() {
        let assets = vec![
            asset("orqella-../evil-linux-x86_64.tar.gz"),
            asset("orqella-9.9.9-linux-x86_64.tar.gz"),
            asset("orqella-9.9.9-macos-arm64.tar.gz"),
            asset("orqella-9.9.9-windows-x86_64.zip"),
            asset("SHA256SUMS.txt"),
        ];
        let linux = platform_asset(&assets, "linux-x86_64", ".tar.gz").unwrap();
        assert_eq!(linux.name, "orqella-9.9.9-linux-x86_64.tar.gz");
        assert_eq!(
            checksum_asset(&assets, &linux).unwrap().name,
            "SHA256SUMS.txt"
        );
        assert!(platform_asset(&assets, "linux-aarch64", ".tar.gz").is_none());
    }

    #[test]
    fn checksum_parser_matches_the_archive_name() {
        let sums = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  *orqella-9.9.9-linux-x86_64.tar.gz
";
        assert_eq!(
            parse_checksum(sums, "orqella-9.9.9-linux-x86_64.tar.gz").unwrap(),
            "b".repeat(64)
        );
        assert!(parse_checksum(sums, "missing.tar.gz").is_err());
    }

    #[test]
    fn signed_manifest_must_match_the_release_version() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let key_id = [1, 2, 3, 4, 5, 6, 7, 8];
        let release = ReleaseCandidate {
            version: Version::new(9, 9, 9),
            tag: "v9.9.9".into(),
            published_at: String::new(),
            notes: String::new(),
            assets: Vec::new(),
        };
        let manifest = serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "version": "9.9.9",
            "commit": "a".repeat(40),
        }))
        .unwrap();
        let signature = sign_manifest(&manifest, &key, key_id, "version:9.9.9");
        let mut config = Config::default();
        config.update.public_key = public_key_base64(&key, key_id);
        verify_release_manifest(&config, &release, &manifest, signature.as_bytes()).unwrap();

        let mut wrong = release.clone();
        wrong.version = Version::new(9, 9, 10);
        assert!(verify_release_manifest(&config, &wrong, &manifest, signature.as_bytes()).is_err());
    }

    #[test]
    fn rollback_protection_uses_the_high_water_mark() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(
            paths.data_dir.join(UPDATE_STATE_FILE),
            json!({"version": "9.9.9", "tag": "v9.9.9", "asset": "x"}).to_string(),
        )
        .unwrap();
        assert!(enforce_forward_update(&paths, &Version::new(9, 9, 8)).is_err());
        assert!(enforce_forward_update(&paths, &Version::new(9, 9, 10)).is_ok());
    }

    #[test]
    fn tar_archive_extracts_the_orqella_binary() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("release.tar.gz");
        let file = std::fs::File::create(&archive).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let contents = b"new binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "orqella-9.9.9-linux-x86_64/orqella",
                &contents[..],
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let output = root.path().join("orqella-new");
        extract_binary(&archive, "release.tar.gz", &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), contents);
    }

    #[test]
    fn zip_archive_extracts_the_orqella_binary() {
        use zip::write::SimpleFileOptions;

        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("release.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("orqella-9.9.9-windows-x86_64/orqella.exe", options)
            .unwrap();
        std::io::Write::write_all(&mut zip, b"new binary").unwrap();
        zip.finish().unwrap();

        let output = root.path().join("orqella-new.exe");
        extract_binary(&archive, "release.zip", &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"new binary");
    }

    #[cfg(unix)]
    #[test]
    fn install_binary_replaces_the_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("orqella");
        std::fs::write(&target, b"old binary").unwrap();
        let new_binary = root.path().join("new");
        std::fs::write(&new_binary, b"new binary").unwrap();
        install_binary(&new_binary, &target).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"new binary");
    }

    #[test]
    fn log_lines_are_capped_to_the_most_recent_characters() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let value = status_value("running", "test", "");
        log_line(&paths, &value, &"x".repeat(40_000));
        let stored = status(&paths);
        assert_eq!(stored["log"].as_str().unwrap().chars().count(), 30_000);
        assert!(stored["log"].as_str().unwrap().ends_with('\n'));
    }
}
