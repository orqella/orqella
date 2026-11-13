use crate::modules::signing::{self, PackageSignature};
use crate::modules::store::InstalledModule;
use anyhow::{Context, Result, bail};
use std::io::{Cursor, Seek, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

pub fn export(module: &InstalledModule) -> Result<Vec<u8>> {
    export_inner(module, None)
}

pub fn export_signed(module: &InstalledModule, key: &ed25519_dalek::SigningKey) -> Result<Vec<u8>> {
    let signature = signing::signature_record(&module.root, key)?;
    export_inner(module, Some(signature))
}

pub fn export_signed_to(
    module: &InstalledModule,
    key: &ed25519_dalek::SigningKey,
    destination: &Path,
) -> Result<()> {
    let signature = signing::signature_record(&module.root, key)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(destination)
        .with_context(|| format!("could not create {}", destination.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    add_dir(&mut zip, &module.root, &module.root, options)?;
    write_bytes(
        &mut zip,
        options,
        signing::SIGNATURE_FILE,
        serde_json::to_vec_pretty(&signature)?.as_slice(),
    )?;
    let file = zip.finish()?;
    file.sync_all()?;
    Ok(())
}

fn export_inner(module: &InstalledModule, signature: Option<PackageSignature>) -> Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    add_dir(&mut zip, &module.root, &module.root, options)?;
    if let Some(signature) = signature {
        write_bytes(
            &mut zip,
            options,
            signing::SIGNATURE_FILE,
            serde_json::to_vec_pretty(&signature)?.as_slice(),
        )?;
    }
    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

fn add_dir<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    root: &Path,
    dir: &Path,
    options: SimpleFileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if (dir == root && name.to_string_lossy().eq_ignore_ascii_case("data"))
            || matches!(
                name.to_string_lossy().as_ref(),
                ".git"
                    | ".config"
                    | ".cache"
                    | ".local"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "__pycache__"
                    | "sidecar.log"
                    | signing::SIGNATURE_FILE
            )
        {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        if entry.file_type()?.is_dir() {
            add_dir(zip, root, &path, options)?;
        } else if entry.file_type()?.is_file() {
            zip.start_file(relative.to_string_lossy().replace('\\', "/"), options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

pub fn import(bytes: &[u8]) -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir().context("could not create an import directory")?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("invalid module package")?;
    if archive.len() > 2_000 {
        bail!("module package has too many entries");
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.size() > 64 * 1024 * 1024 {
            bail!("module package entry is too large");
        }
        total = total.saturating_add(file.size());
        if total > 128 * 1024 * 1024 {
            bail!("module package expands beyond the size limit");
        }
        let Some(path) = file.enclosed_name() else {
            bail!("module package contains an unsafe path");
        };
        let target = dir.path().join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if file.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            let mut out = std::fs::File::create(&target)?;
            std::io::copy(&mut file, &mut out)?;
        }
    }
    Ok(dir)
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
    use crate::modules::manifest::Manifest;
    use crate::modules::store::ModuleState;
    use std::io::Read;

    #[test]
    fn export_omits_runtime_data() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "page").unwrap();
        std::fs::create_dir(root.path().join("data")).unwrap();
        std::fs::create_dir(root.path().join(".config")).unwrap();
        std::fs::write(root.path().join("data/secret.json"), "secret").unwrap();
        std::fs::write(root.path().join(".config/secret"), "secret").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "test.export"
name = "Export"
version = "0.1.0"
api_version = "1"
entry = "ui/index.html"
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        let module = InstalledModule {
            manifest,
            root: root.path().to_path_buf(),
            state: ModuleState::default(),
        };
        let bytes = export(&module).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("ui/index.html").is_ok());
        assert!(archive.by_name("data/secret.json").is_err());
        assert!(archive.by_name(".config/secret").is_err());
        let mut page = archive.by_name("ui/index.html").unwrap();
        let mut content = String::new();
        page.read_to_string(&mut content).unwrap();
        assert_eq!(content, "page");

        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let signed = export_signed(&module, &key).unwrap();
        let imported = import(&signed).unwrap();
        let trusted = vec![crate::config::TrustedKey {
            public_key: signing::public_key_hex(&key),
            label: "test".into(),
        }];
        signing::verify_tree(imported.path(), &trusted, false, false).unwrap();
    }

    #[test]
    fn unsigned_packages_are_importable_but_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "page").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "test.unsigned"
name = "Unsigned"
version = "0.1.0"
api_version = "1"
entry = "ui/index.html"
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        let module = InstalledModule {
            manifest,
            root: root.path().to_path_buf(),
            state: ModuleState::default(),
        };
        let bytes = export(&module).unwrap();
        let imported = import(&bytes).unwrap();
        assert!(signing::verify_tree(imported.path(), &[], false, false).is_err());
        assert!(
            !signing::verify_tree(imported.path(), &[], true, false)
                .unwrap()
                .signed
        );
    }

    #[test]
    fn import_rejects_unsafe_zip_paths() {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut buffer);
            zip.start_file("../escape.txt", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"escape").unwrap();
            zip.finish().unwrap();
        }
        let error = import(buffer.get_ref()).unwrap_err();
        assert!(format!("{error:#}").contains("unsafe path"));
    }

    #[test]
    fn digest_ignores_runtime_directories_and_signature_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "page").unwrap();
        let baseline = signing::digest_tree(root.path()).unwrap();
        std::fs::create_dir(root.path().join("data")).unwrap();
        std::fs::write(root.path().join("data/secret"), "runtime").unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/config"), "secret").unwrap();
        assert_eq!(signing::digest_tree(root.path()).unwrap(), baseline);
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        signing::sign_tree(root.path(), &key).unwrap();
        assert_eq!(signing::digest_tree(root.path()).unwrap(), baseline);
        assert!(signing::read_signature(root.path()).unwrap().is_some());
    }

    #[test]
    fn signed_packages_can_be_verified_with_an_untrusted_override() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "page").unwrap();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        signing::sign_tree(root.path(), &key).unwrap();
        let report = signing::verify_tree(root.path(), &[], false, true).unwrap();
        assert!(report.signed);
        assert!(!report.trusted);
        assert_eq!(
            report.public_key.as_deref(),
            Some(signing::public_key_hex(&key).as_str())
        );
    }
}
