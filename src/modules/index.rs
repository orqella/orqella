//! Static module index and index-backed installation.
//!
//! The index is a curated JSON document. Each module version distributes an
//! immutable source (a git commit or a release archive) and the SHA-256 digest
//! of the extracted module tree. The daemon verifies the digest before using
//! the tree, so an index install does not depend on a publisher signature.

use crate::modules::git;
use crate::modules::package;
use crate::modules::store::{ModuleStore, ModuleView};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

pub const INDEX_SCHEMA: u32 = 1;
pub const MODULE_API_VERSION: &str = "1";

const INDEX_MAX_BYTES: u64 = 2 * 1024 * 1024;
const ARCHIVE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const ARCHIVE_MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const ARCHIVE_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const ARCHIVE_MAX_ENTRIES: usize = 2_000;
const INDEX_TIMEOUT_SECS: u64 = 20;
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleIndex {
    pub schema: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub modules: Vec<IndexModule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexModule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub publisher: Publisher,
    #[serde(default)]
    pub official: bool,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default = "default_node_scope")]
    pub node_scope: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub versions: Vec<IndexVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publisher {
    pub name: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVersion {
    pub version: String,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default)]
    pub released_at: String,
    #[serde(default)]
    pub notes_url: String,
    #[serde(default)]
    pub distributions: Vec<Distribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Distribution {
    /// Empty means every supported platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    pub source: DistributionSource,
    pub tree_sha256: String,
    #[serde(default)]
    pub backend: Option<IndexBackend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DistributionSource {
    Git {
        url: String,
        #[serde(default, rename = "ref")]
        reference: String,
        commit: String,
    },
    Archive {
        url: String,
        format: String,
        sha256: String,
        #[serde(default)]
        size: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexBackend {
    #[serde(default = "default_backend_kind")]
    pub kind: String,
    #[serde(default)]
    pub compiled: bool,
    #[serde(default)]
    pub platforms: Vec<String>,
}

fn default_node_scope() -> String {
    "node".into()
}

fn default_api_version() -> String {
    MODULE_API_VERSION.into()
}

fn default_backend_kind() -> String {
    "none".into()
}

/// A module version plus the distribution selected for this host.
#[derive(Debug, Clone)]
pub struct InstallTarget {
    pub id: String,
    pub name: String,
    pub version: String,
    pub distribution: Distribution,
}

#[derive(Debug, Serialize)]
pub struct CatalogView {
    pub schema: u32,
    pub platform: String,
    pub api_version: String,
    pub index_url: String,
    pub modules: Vec<CatalogModule>,
}

#[derive(Debug, Serialize)]
pub struct CatalogModule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub publisher: Publisher,
    pub official: bool,
    pub homepage: String,
    pub license: String,
    pub node_scope: String,
    pub permissions: Vec<String>,
    pub latest_version: Option<String>,
    pub latest_released_at: Option<String>,
    pub notes_url: Option<String>,
    pub platforms: Vec<String>,
    pub compiled_backend: bool,
    pub backend_platforms: Vec<String>,
    pub compatible: bool,
    pub incompatible_reason: Option<String>,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub installed_source: Option<String>,
    pub update_available: bool,
    pub source_type: Option<String>,
}

impl ModuleIndex {
    pub fn validate(&self) -> Result<()> {
        if self.schema != INDEX_SCHEMA {
            bail!(
                "unsupported module index schema {} (expected {INDEX_SCHEMA})",
                self.schema
            );
        }
        let mut ids = HashSet::new();
        for module in &self.modules {
            if !ids.insert(module.id.clone()) {
                bail!("duplicate module id in index: {}", module.id);
            }
            crate::util::validate_module_id(&module.id)?;
            if module.name.trim().is_empty() {
                bail!("module {} has an empty name", module.id);
            }
            if module.publisher.name.trim().is_empty() {
                bail!("module {} has an empty publisher", module.id);
            }
            let mut versions = HashSet::new();
            for version in &module.versions {
                if version.version.trim().is_empty() {
                    bail!("module {} has an empty version", module.id);
                }
                if version.api_version.trim().is_empty() {
                    bail!(
                        "module {} {} has an empty api_version",
                        module.id,
                        version.version
                    );
                }
                if !versions.insert(version.version.clone()) {
                    bail!(
                        "module {} lists version {} more than once",
                        module.id,
                        version.version
                    );
                }
                if version.distributions.is_empty() {
                    bail!(
                        "module {} {} has no distributions",
                        module.id,
                        version.version
                    );
                }
                for distribution in &version.distributions {
                    validate_distribution(module, version, distribution)?;
                }
            }
        }
        Ok(())
    }

    pub fn module(&self, id: &str) -> Option<&IndexModule> {
        self.modules.iter().find(|module| module.id == id)
    }

    /// Select the newest version compatible with the daemon API and host.
    pub fn resolve(
        &self,
        id: &str,
        requested_version: Option<&str>,
        platform: &str,
        api_version: &str,
    ) -> Result<InstallTarget> {
        let module = self
            .module(id)
            .with_context(|| format!("module {id} is not in the index"))?;
        let selected = select_version(module, requested_version, platform, api_version)
            .with_context(|| {
                incompatible_reason(module, requested_version, platform, api_version)
            })?;
        Ok(InstallTarget {
            id: module.id.clone(),
            name: module.name.clone(),
            version: selected.version.version.clone(),
            distribution: selected.distribution.clone(),
        })
    }

    pub fn catalog(
        &self,
        store: &ModuleStore,
        platform: &str,
        api_version: &str,
        index_url: &str,
    ) -> CatalogView {
        let mut modules: Vec<CatalogModule> = self
            .modules
            .iter()
            .map(|module| {
                let selected = select_version(module, None, platform, api_version);
                let latest = selected
                    .as_ref()
                    .map(|selected| selected.version)
                    .or_else(|| {
                        module
                            .versions
                            .iter()
                            .max_by(|a, b| version_cmp(&a.version, &b.version))
                    });
                let installed = store.get(&module.id);
                let installed_version = installed
                    .as_ref()
                    .map(|entry| entry.manifest.version.clone());
                let installed_source = installed.map(|entry| entry.state.source.clone());
                let update_available = selected
                    .as_ref()
                    .zip(installed_version.as_ref())
                    .is_some_and(|(selected, installed)| {
                        version_cmp(&selected.version.version, installed) == Ordering::Greater
                    });
                let backend = selected
                    .as_ref()
                    .and_then(|selected| selected.distribution.backend.clone());
                CatalogModule {
                    id: module.id.clone(),
                    name: module.name.clone(),
                    description: module.description.clone(),
                    category: module.category.clone(),
                    tags: module.tags.clone(),
                    publisher: module.publisher.clone(),
                    official: module.official,
                    homepage: module.homepage.clone(),
                    license: module.license.clone(),
                    node_scope: module.node_scope.clone(),
                    permissions: module.permissions.clone(),
                    latest_version: latest.map(|version| version.version.clone()),
                    latest_released_at: latest
                        .map(|version| version.released_at.clone())
                        .filter(|value| !value.is_empty()),
                    notes_url: latest
                        .map(|version| version.notes_url.clone())
                        .filter(|value| !value.is_empty()),
                    platforms: selected
                        .as_ref()
                        .map(|selected| selected.distribution.platforms.clone())
                        .unwrap_or_default(),
                    compiled_backend: backend.as_ref().is_some_and(|backend| backend.compiled),
                    backend_platforms: backend.map(|backend| backend.platforms).unwrap_or_default(),
                    compatible: selected.is_some(),
                    incompatible_reason: selected
                        .is_none()
                        .then(|| incompatible_reason(module, None, platform, api_version)),
                    installed: installed_version.is_some(),
                    installed_version,
                    installed_source,
                    update_available,
                    source_type: selected
                        .as_ref()
                        .map(|selected| source_type(&selected.distribution.source)),
                }
            })
            .collect();
        modules.sort_by(|a, b| {
            b.compatible
                .cmp(&a.compatible)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        CatalogView {
            schema: self.schema,
            platform: platform.into(),
            api_version: api_version.into(),
            index_url: index_url.into(),
            modules,
        }
    }
}

fn select_version<'a>(
    module: &'a IndexModule,
    requested: Option<&str>,
    platform: &str,
    api_version: &str,
) -> Option<SelectedVersion<'a>> {
    let mut candidates: Vec<SelectedVersion<'a>> = module
        .versions
        .iter()
        .filter(|version| version.api_version == api_version)
        .filter(|version| requested.is_none_or(|requested| version.version == requested))
        .flat_map(|version| {
            version
                .distributions
                .iter()
                .filter(|distribution| distribution.supports(platform))
                .map(|distribution| SelectedVersion {
                    version,
                    distribution,
                })
        })
        .collect();
    candidates.sort_by(|a, b| version_cmp(&b.version.version, &a.version.version));
    candidates.into_iter().next()
}

struct SelectedVersion<'a> {
    version: &'a IndexVersion,
    distribution: &'a Distribution,
}

fn incompatible_reason(
    module: &IndexModule,
    requested: Option<&str>,
    platform: &str,
    api_version: &str,
) -> String {
    let matching_api = module.versions.iter().any(|version| {
        version.api_version == api_version
            && requested.is_none_or(|requested| version.version == requested)
    });
    if !matching_api {
        return format!(
            "{} does not provide a version for module API {api_version}",
            module.name
        );
    }
    format!("{} is not available for {platform}", module.name)
}

fn source_type(source: &DistributionSource) -> String {
    match source {
        DistributionSource::Git { .. } => "git".into(),
        DistributionSource::Archive { .. } => "archive".into(),
    }
}

impl Distribution {
    pub fn supports(&self, platform: &str) -> bool {
        self.platforms.is_empty()
            || self
                .platforms
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(platform))
    }
}

fn validate_distribution(
    module: &IndexModule,
    version: &IndexVersion,
    distribution: &Distribution,
) -> Result<()> {
    if !is_sha256(&distribution.tree_sha256) {
        bail!(
            "module {} {} has an invalid tree_sha256",
            module.id,
            version.version
        );
    }
    for platform in &distribution.platforms {
        validate_platform(platform)?;
    }
    if let Some(backend) = &distribution.backend {
        if !matches!(backend.kind.as_str(), "none" | "sidecar") {
            bail!(
                "module {} {} has an invalid backend kind",
                module.id,
                version.version
            );
        }
        for platform in &backend.platforms {
            validate_platform(platform)?;
        }
    }
    match &distribution.source {
        DistributionSource::Git { url, commit, .. } => {
            validate_remote_url(url)?;
            if commit.len() < 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!(
                    "module {} {} has an invalid git commit",
                    module.id,
                    version.version
                );
            }
        }
        DistributionSource::Archive {
            url,
            format,
            sha256,
            ..
        } => {
            validate_remote_url(url)?;
            if !matches!(format.as_str(), "zip" | "tar.gz") {
                bail!(
                    "module {} {} uses unsupported archive format {format}",
                    module.id,
                    version.version
                );
            }
            if !is_sha256(sha256) {
                bail!(
                    "module {} {} has an invalid archive sha256",
                    module.id,
                    version.version
                );
            }
        }
    }
    Ok(())
}

fn validate_platform(platform: &str) -> Result<()> {
    let Some((os, arch)) = platform.split_once('-') else {
        bail!("module index platform must be <os>-<arch>, for example linux-x86_64");
    };
    if !matches!(os, "linux" | "macos" | "windows") {
        bail!("unknown module index operating system `{os}`");
    }
    if arch.is_empty() || !arch.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid module index architecture `{arch}`");
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_remote_url(url: &str) -> Result<()> {
    if remote_url_allowed(url) {
        Ok(())
    } else {
        bail!("module index URLs must use HTTPS (or loopback HTTP for local testing): {url}")
    }
}

fn remote_url_allowed(url: &str) -> bool {
    let value = url.trim();
    if let Some(rest) = value.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let host = rest
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "127.0.0.1" || host == "localhost"
}

fn version_cmp(left: &str, right: &str) -> Ordering {
    let mut left_parts = version_parts(left);
    let mut right_parts = version_parts(right);
    let width = left_parts.len().max(right_parts.len()).max(3);
    left_parts.resize(width, 0);
    right_parts.resize(width, 0);
    let core = left_parts.cmp(&right_parts);
    if core != Ordering::Equal {
        return core;
    }
    let left_pre = left.contains('-');
    let right_pre = right.contains('-');
    match (left_pre, right_pre) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => left.cmp(right),
    }
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .split('-')
        .next()
        .unwrap_or(value)
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

pub async fn fetch(client: &reqwest::Client, index_url: &str) -> Result<ModuleIndex> {
    let bytes = download_limited(
        client,
        index_url,
        INDEX_MAX_BYTES,
        INDEX_TIMEOUT_SECS,
        "module index",
    )
    .await?;
    let index: ModuleIndex =
        serde_json::from_slice(&bytes).context("module index is not valid JSON")?;
    index.validate()?;
    Ok(index)
}

pub async fn install(
    client: &reqwest::Client,
    store: Arc<ModuleStore>,
    target: InstallTarget,
) -> Result<ModuleView> {
    let InstallTarget {
        id,
        version,
        distribution,
        ..
    } = target;
    let expected_digest = distribution.tree_sha256.clone();
    match distribution.source {
        DistributionSource::Git { url, commit, .. } => {
            if crate::util::git_available() {
                tokio::task::spawn_blocking(move || {
                    let temp = tempfile::tempdir().context("could not create a module checkout")?;
                    let checkout = temp.path().join("checkout");
                    git::clone_at_commit(&url, &commit, &checkout)?;
                    store.install_from_index_tree(&checkout, true, &id, &version, &expected_digest)
                })
                .await
                .context("module install worker failed")?
            } else {
                // The installer promises no Git requirement. A GitHub git
                // distribution still maps to an immutable commit archive, so
                // fetch that and verify the same pinned tree digest.
                let archive_url = github_archive_url(&url, &commit)
                    .context("Git is required to install this module")?;
                let bytes = download_limited(
                    client,
                    &archive_url,
                    ARCHIVE_MAX_BYTES,
                    DOWNLOAD_TIMEOUT_SECS,
                    "module archive",
                )
                .await?;
                install_archive_bytes(store, &id, &version, &expected_digest, "tar.gz", bytes).await
            }
        }
        DistributionSource::Archive {
            url,
            format,
            sha256,
            size,
        } => {
            let bytes = download_archive(client, &url, &sha256, size).await?;
            install_archive_bytes(store, &id, &version, &expected_digest, &format, bytes).await
        }
    }
}

/// Install a verified module tree from archive bytes.
///
/// Used by both archive distributions and the GitHub commit-archive fallback
/// when Git is not available. The caller is responsible for any archive
/// checksum; `tree_sha256` is always verified against the extracted tree
/// before installation.
pub async fn install_archive_bytes(
    store: Arc<ModuleStore>,
    id: &str,
    version: &str,
    tree_sha256: &str,
    format: &str,
    bytes: Vec<u8>,
) -> Result<ModuleView> {
    let id = id.to_string();
    let version = version.to_string();
    let tree_sha256 = tree_sha256.to_string();
    let format = format.to_string();
    tokio::task::spawn_blocking(move || {
        let temp = extract_archive(&bytes, &format)?;
        store.install_from_index_tree(temp.path(), false, &id, &version, &tree_sha256)
    })
    .await
    .context("module install worker failed")?
}

/// Map a GitHub git source to the immutable commit archive used when Git is
/// unavailable. The extracted tree digest is still verified by
/// `install_from_index_tree`.
pub fn github_archive_url(url: &str, commit: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.host_str(), Some("github.com" | "www.github.com")) {
        return None;
    }
    let path = parsed.path().trim_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!(
        "https://codeload.github.com/{owner}/{repo}/tar.gz/{commit}"
    ))
}

async fn download_archive(
    client: &reqwest::Client,
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<Vec<u8>> {
    let bytes = download_limited(
        client,
        url,
        ARCHIVE_MAX_BYTES,
        DOWNLOAD_TIMEOUT_SECS,
        "module archive",
    )
    .await?;
    if expected_size > 0 && bytes.len() as u64 != expected_size {
        bail!(
            "module archive size is {} bytes, expected {expected_size}",
            bytes.len()
        );
    }
    let actual = hex::encode(Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        bail!("module archive checksum does not match the module index");
    }
    Ok(bytes)
}

async fn download_limited(
    client: &reqwest::Client,
    url: &str,
    max_bytes: u64,
    timeout_secs: u64,
    label: &str,
) -> Result<Vec<u8>> {
    validate_remote_url(url)?;
    let response = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("orqella/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
        .with_context(|| format!("could not download {label}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{label} download returned {status}");
    }
    if !remote_url_allowed(response.url().as_str()) {
        bail!("{label} redirected outside HTTPS");
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        bail!("{label} exceeds the {} MB limit", max_bytes / (1024 * 1024));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("{label} download was interrupted"))?;
        total = total.saturating_add(chunk.len() as u64);
        if total > max_bytes {
            bail!("{label} exceeds the {} MB limit", max_bytes / (1024 * 1024));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn extract_archive(bytes: &[u8], format: &str) -> Result<TempDir> {
    match format {
        "zip" => package::import(bytes),
        "tar.gz" => extract_tar_gz(bytes),
        other => bail!("unsupported module archive format {other}"),
    }
}

fn extract_tar_gz(bytes: &[u8]) -> Result<TempDir> {
    let dir = tempfile::tempdir().context("could not create a module archive directory")?;
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut entries = 0usize;
    let mut total = 0u64;
    for entry in archive.entries().context("invalid module archive")? {
        let mut entry = entry.context("invalid module archive entry")?;
        entries += 1;
        if entries > ARCHIVE_MAX_ENTRIES {
            bail!("module archive has too many entries");
        }
        let size = entry.header().size().unwrap_or(0);
        if size > ARCHIVE_MAX_ENTRY_BYTES {
            bail!("module archive entry is too large");
        }
        total = total.saturating_add(size);
        if total > ARCHIVE_MAX_TOTAL_BYTES {
            bail!("module archive expands beyond the size limit");
        }
        if !entry
            .unpack_in(dir.path())
            .context("could not extract module archive entry")?
        {
            bail!("module archive contains an unsafe path");
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_git_sources_have_a_commit_archive() {
        assert_eq!(
            github_archive_url("https://github.com/orqella/orqella-agent-harness", "abc123")
                .as_deref(),
            Some("https://codeload.github.com/orqella/orqella-agent-harness/tar.gz/abc123")
        );
        assert_eq!(
            github_archive_url("https://github.com/acme/repo.git", "abc123").as_deref(),
            Some("https://codeload.github.com/acme/repo/tar.gz/abc123")
        );
        assert_eq!(
            github_archive_url("https://example.com/org/repo", "abc123"),
            None
        );
    }

    fn index() -> ModuleIndex {
        let json = r#"
        {
          "schema": 1,
          "modules": [
            {
              "id": "demo",
              "name": "Demo",
              "publisher": { "name": "Test" },
              "versions": [
                {
                  "version": "0.1.0",
                  "api_version": "1",
                  "distributions": [
                    {
                      "platforms": ["linux-x86_64"],
                      "source": {
                        "type": "git",
                        "url": "https://example.test/demo",
                        "ref": "main",
                        "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                      },
                      "tree_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }
                  ]
                },
                {
                  "version": "0.2.0",
                  "api_version": "1",
                  "distributions": [
                    {
                      "platforms": ["linux-x86_64"],
                      "source": {
                        "type": "git",
                        "url": "https://example.test/demo",
                        "ref": "main",
                        "commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                      },
                      "tree_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    },
                    {
                      "platforms": ["macos-aarch64"],
                      "source": {
                        "type": "git",
                        "url": "https://example.test/demo",
                        "ref": "main",
                        "commit": "cccccccccccccccccccccccccccccccccccccccc"
                      },
                      "tree_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                    }
                  ]
                }
              ]
            }
          ]
        }
        "#;
        let index: ModuleIndex = serde_json::from_str(json).unwrap();
        index.validate().unwrap();
        index
    }

    #[test]
    fn selects_the_newest_compatible_version() {
        let index = index();
        let target = index.resolve("demo", None, "linux-x86_64", "1").unwrap();
        assert_eq!(target.version, "0.2.0");
        let target = index.resolve("demo", None, "macos-aarch64", "1").unwrap();
        assert_eq!(target.version, "0.2.0");
    }

    #[test]
    fn rejects_incompatible_platforms_and_api_versions() {
        let index = index();
        assert!(index.resolve("demo", None, "windows-x86_64", "1").is_err());
        assert!(index.resolve("demo", None, "linux-x86_64", "2").is_err());
        assert!(index.resolve("missing", None, "linux-x86_64", "1").is_err());
    }

    #[test]
    fn validates_archive_distributions_and_digests() {
        let json = r#"
        {
          "schema": 1,
          "modules": [{
            "id": "demo",
            "name": "Demo",
            "publisher": {"name": "Test"},
            "versions": [{
              "version": "1.0.0",
              "distributions": [{
                "source": {
                  "type": "archive",
                  "url": "https://example.test/demo.zip",
                  "format": "zip",
                  "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "tree_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
              }]
            }]
          }]
        }
        "#;
        let index: ModuleIndex = serde_json::from_str(json).unwrap();
        index.validate().unwrap();
        let mut broken = index.clone();
        broken.modules[0].versions[0].distributions[0].tree_sha256 = "short".into();
        assert!(broken.validate().is_err());
    }

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(version_cmp("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(version_cmp("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.0.0-rc.1", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn only_allows_https_or_exact_loopback_http_sources() {
        assert!(remote_url_allowed("https://example.test/index.json"));
        assert!(remote_url_allowed("http://127.0.0.1:4181/index.json"));
        assert!(remote_url_allowed("http://localhost/index.json"));
        assert!(!remote_url_allowed("http://127.0.0.1.evil.test/index.json"));
        assert!(!remote_url_allowed("http://example.test/index.json"));
        assert!(!remote_url_allowed("file:///tmp/index.json"));
        assert!(!remote_url_allowed("https:///index.json"));
    }

    #[test]
    fn extracts_a_tar_gz_module_archive() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use tar::{Builder, Header};

        let mut bytes = Vec::new();
        {
            let encoder = GzEncoder::new(&mut bytes, Compression::fast());
            let mut archive = Builder::new(encoder);
            let payload = b"schema = 1\nid = \"test.archive\"\nname = \"Archive\"\nversion = \"1.0.0\"\napi_version = \"1\"\n";
            let mut header = Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "module.toml", payload.as_slice())
                .unwrap();
            archive.finish().unwrap();
        }
        let temp = extract_archive(&bytes, "tar.gz").unwrap();
        assert!(temp.path().join("module.toml").is_file());
    }
}
