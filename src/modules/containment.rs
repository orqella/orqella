//! Platform sidecar containment helpers.
//!
//! Linux uses bubblewrap from the sidecar launcher. macOS uses sandbox-exec.
//! Windows uses a Job Object for resource and lifecycle containment; it does
//! not provide filesystem isolation.

#[cfg(windows)]
use anyhow::{Result, bail};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

#[cfg(windows)]
pub struct JobGuard(HANDLE);

#[cfg(windows)]
unsafe impl Send for JobGuard {}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
pub fn create_job(max_memory_mb: u64) -> Result<JobGuard> {
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            bail!("could not create a Windows Job Object");
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = max_memory_mb.saturating_mul(1024 * 1024) as usize;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            CloseHandle(handle);
            bail!("could not configure the Windows Job Object");
        }
        Ok(JobGuard(handle))
    }
}

#[cfg(windows)]
pub fn assign_job(job: &JobGuard, child: &tokio::process::Child) -> Result<()> {
    let handle = child
        .raw_handle()
        .ok_or_else(|| anyhow::anyhow!("sidecar process has no Windows handle"))?;
    unsafe {
        if AssignProcessToJobObject(job.0, handle as HANDLE) == 0 {
            bail!("could not assign sidecar to the Windows Job Object");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn profile_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
pub fn macos_profile(
    root: &std::path::Path,
    read_paths: &[String],
    write_paths: &[String],
) -> String {
    let root = profile_path(root);
    let extra = read_paths
        .iter()
        .filter_map(|path| {
            if let Some(rest) = path.strip_prefix("~/") {
                dirs::home_dir().map(|home| home.join(rest))
            } else {
                Some(std::path::PathBuf::from(path))
            }
        })
        .map(|path| format!("(subpath \"{}\")", profile_path(&path)))
        .collect::<Vec<_>>()
        .join(" ");
    let extra_write = write_paths
        .iter()
        .map(|path| format!("(subpath \"{}\")", profile_path(std::path::Path::new(path))))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow sysctl-read)
(allow mach-lookup)
(allow file-read-metadata (subpath "/"))
(allow file-read*
  (subpath "/usr") (subpath "/bin") (subpath "/System") (subpath "/Library")
  (subpath "/private/etc") (subpath "/dev") (subpath "/private/tmp")
  (subpath "/private/var/folders")
  (literal "/var/db/xcode_select_link")
  (literal "/private/var/db/xcode_select_link")
  {extra} {extra_write} (subpath "{root}"))
(allow file-write* (subpath "{root}") (subpath "/private/tmp") (subpath "/private/var/folders") {extra_write})
(deny file-write* (subpath "{root}/.git") (literal "{root}/module.toml"))
(allow network*)
"#
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_profile_allows_command_line_tools_python() {
        let profile = macos_profile(std::path::Path::new("/tmp/module"), &[], &[]);
        assert!(profile.contains("file-read-metadata"));
        assert!(profile.contains("/private/var/folders"));
        assert!(profile.contains("/var/db/xcode_select_link"));
        assert!(profile.contains("subpath \"/tmp/module\""));
    }
}
