pub mod containment;
pub mod git;
pub mod manifest;
pub mod package;
pub mod sidecar;
pub mod signing;
pub mod store;

pub use manifest::Manifest;
pub use sidecar::SidecarManager;
pub use store::ModuleStore;

/// Return the module's runtime data directory, creating it if needed.
///
/// `data/` is preserved across updates and may contain credentials and user
/// files. Refuse a symlink (or non-directory) instead of following it; a module
/// must not be able to redirect daemon-side artifact/venv writes outside its
/// root.
pub fn ensure_module_data_dir(root: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let data = root.join("data");
    match std::fs::symlink_metadata(&data) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("module data directory is a symlink");
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!("module data path is not a directory");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&data)?;
        }
        Err(error) => return Err(error.into()),
    }
    let canonical_root = root.canonicalize()?;
    let canonical_data = data.canonicalize()?;
    if !canonical_data.starts_with(&canonical_root) {
        anyhow::bail!("module data directory is outside the module root");
    }
    Ok(canonical_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_data_directory_is_created_and_cannot_be_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let data = ensure_module_data_dir(root.path()).unwrap();
        assert!(data.is_dir());
        assert!(data.starts_with(root.path().canonicalize().unwrap()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let other = tempfile::tempdir().unwrap();
            let link_root = tempfile::tempdir().unwrap();
            symlink(other.path(), link_root.path().join("data")).unwrap();
            assert!(ensure_module_data_dir(link_root.path()).is_err());
        }
    }
}
