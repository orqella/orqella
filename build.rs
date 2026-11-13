use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-env-changed=ORQELLA_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ORQELLA_BUILD_TIME");
    if std::env::var_os("ORQELLA_BUILD_COMMIT").is_none()
        && let Ok(output) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
        && output.status.success()
    {
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !commit.is_empty() {
            println!("cargo:rustc-env=ORQELLA_BUILD_COMMIT={commit}");
        }
    }
}
