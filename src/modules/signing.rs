use crate::config::{Paths, TrustedKey};
use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const SIGNATURE_FILE: &str = "ORQELLA_SIGNATURE.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSignature {
    pub schema: u32,
    pub algorithm: String,
    pub public_key: String,
    pub fingerprint: String,
    pub digest: String,
    pub signature: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustReport {
    pub signed: bool,
    pub trusted: bool,
    pub fingerprint: Option<String>,
    pub public_key: Option<String>,
}

pub fn key_path(paths: &Paths) -> PathBuf {
    paths.data_dir.join("module-signing.key")
}

pub fn load_or_create_key(paths: &Paths) -> Result<SigningKey> {
    let path = key_path(paths);
    if path.exists() {
        let _ = crate::util::restrict_file(&path);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let bytes = hex::decode(raw.trim()).context("invalid module signing key")?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("module signing key must be 32 bytes"))?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let key = SigningKey::generate(&mut OsRng);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::util::write_atomic(&path, hex::encode(key.to_bytes()).as_bytes())?;
    crate::util::restrict_file(&path)?;
    Ok(key)
}

pub fn public_key_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

pub fn fingerprint(public_key_hex: &str) -> String {
    let digest = Sha256::digest(public_key_hex.as_bytes());
    format!("ed25519:{}", hex::encode(digest))
}

pub fn signature_record(root: &Path, key: &SigningKey) -> Result<PackageSignature> {
    let digest = digest_tree(root)?;
    let signature = key.sign(digest.as_bytes());
    let public_key = public_key_hex(key);
    Ok(PackageSignature {
        schema: 1,
        algorithm: "ed25519".into(),
        fingerprint: fingerprint(&public_key),
        public_key,
        digest,
        signature: hex::encode(signature.to_bytes()),
        created_at: crate::util::now_iso(),
    })
}

pub fn sign_tree(root: &Path, key: &SigningKey) -> Result<PackageSignature> {
    let record = signature_record(root, key)?;
    let raw = serde_json::to_vec_pretty(&record)?;
    crate::util::write_atomic(&root.join(SIGNATURE_FILE), &raw)?;
    Ok(record)
}

pub fn verify_tree(
    root: &Path,
    trusted: &[TrustedKey],
    allow_unsigned: bool,
    allow_untrusted: bool,
) -> Result<TrustReport> {
    let path = root.join(SIGNATURE_FILE);
    if !path.is_file() {
        if allow_unsigned {
            return Ok(TrustReport {
                signed: false,
                trusted: false,
                fingerprint: None,
                public_key: None,
            });
        }
        bail!("module package is unsigned; pass --allow-unsigned only for trusted local packages");
    }
    let raw = std::fs::read_to_string(&path)?;
    let record: PackageSignature =
        serde_json::from_str(&raw).context("invalid package signature")?;
    if record.schema != 1 || record.algorithm != "ed25519" {
        bail!("unsupported package signature format");
    }
    let public_bytes = hex::decode(&record.public_key).context("invalid package public key")?;
    let public_bytes: [u8; 32] = public_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid package public key length"))?;
    let verifying =
        VerifyingKey::from_bytes(&public_bytes).context("invalid package public key")?;
    let signature_bytes = hex::decode(&record.signature).context("invalid package signature")?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid package signature length"))?;
    let signature = Signature::from_bytes(&signature_bytes);
    let digest = digest_tree(root)?;
    if digest != record.digest {
        bail!("package signature does not match package contents");
    }
    verifying
        .verify_strict(record.digest.as_bytes(), &signature)
        .context("package signature verification failed")?;
    let trusted_match = trusted
        .iter()
        .any(|key| key.public_key.eq_ignore_ascii_case(&record.public_key));
    if !trusted_match && !allow_untrusted {
        bail!(
            "package is signed by untrusted publisher {}; trust it with `orqella module trust {}`",
            record.fingerprint,
            record.public_key
        );
    }
    Ok(TrustReport {
        signed: true,
        trusted: trusted_match,
        fingerprint: Some(record.fingerprint),
        public_key: Some(record.public_key),
    })
}

pub fn read_signature(root: &Path) -> Result<Option<PackageSignature>> {
    let path = root.join(SIGNATURE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(path)?)?))
}

pub fn digest_tree(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (relative, path) in files {
        let bytes = std::fs::read(path)?;
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (dir == root && name.eq_ignore_ascii_case("data"))
            || matches!(
                name.as_ref(),
                ".git"
                    | ".config"
                    | ".cache"
                    | ".local"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "__pycache__"
                    | "sidecar.log"
                    | SIGNATURE_FILE
            )
        {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, files)?;
        } else if entry.file_type()?.is_file() {
            let relative = path.strip_prefix(root)?;
            files.push((relative.to_string_lossy().replace('\\', "/"), path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_verifies_and_rejects_tampering() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        let key = SigningKey::generate(&mut OsRng);
        let record = sign_tree(root.path(), &key).unwrap();
        let trusted = vec![TrustedKey {
            public_key: public_key_hex(&key),
            label: "test".into(),
        }];
        let report = verify_tree(root.path(), &trusted, false, false).unwrap();
        assert!(report.trusted);
        assert_eq!(
            report.fingerprint.as_deref(),
            Some(record.fingerprint.as_str())
        );

        std::fs::write(root.path().join("ui/index.html"), "tampered").unwrap();
        assert!(verify_tree(root.path(), &trusted, false, false).is_err());
    }

    #[test]
    fn unsigned_packages_require_an_explicit_local_override() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        assert!(verify_tree(root.path(), &[], false, false).is_err());
        let report = verify_tree(root.path(), &[], true, false).unwrap();
        assert!(!report.signed);
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
    fn signing_key_is_created_once_with_private_permissions() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let first = load_or_create_key(&paths).unwrap();
        let second = load_or_create_key(&paths).unwrap();
        assert_eq!(public_key_hex(&first), public_key_hex(&second));
        assert_eq!(
            fingerprint(&public_key_hex(&first)),
            fingerprint(&public_key_hex(&second))
        );
        assert!(fingerprint(&public_key_hex(&first)).starts_with("ed25519:"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(key_path(&paths))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupted_signing_keys_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(key_path(&paths), "not hex").unwrap();
        assert!(load_or_create_key(&paths).is_err());
        std::fs::write(key_path(&paths), "aa").unwrap();
        assert!(load_or_create_key(&paths).is_err());
    }

    #[test]
    fn signature_metadata_rejects_tampered_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        let key = SigningKey::generate(&mut OsRng);
        sign_tree(root.path(), &key).unwrap();
        let raw = std::fs::read_to_string(root.path().join(SIGNATURE_FILE)).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["algorithm"] = serde_json::json!("rsa");
        std::fs::write(
            root.path().join(SIGNATURE_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(verify_tree(root.path(), &[], true, true).is_err());
    }
}
