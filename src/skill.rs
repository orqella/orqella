//! The bundled Orqella agent skill and the directories agents discover it in.
//!
//! OpenCode reads `~/.agents/skills` globally (and project `.agents/skills`),
//! while Claude Code reads `~/.claude/skills`. Installing the same `SKILL.md`
//! in both places keeps the guide available without a source checkout.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const SKILL: &str = include_str!("../skills/orqella/SKILL.md");

/// The shared agent-skills directory. OpenCode and other compatible agents
/// read this globally; Claude Code uses its own directory below.
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".agents/skills/orqella"))
}

fn claude_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let claude = home.join(".claude");
    claude
        .is_dir()
        .then(|| claude.join("skills").join("orqella"))
}

fn opencode_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let opencode = home.join(".config").join("opencode");
    opencode
        .is_dir()
        .then(|| opencode.join("skills").join("orqella"))
}

/// Every standard discovery directory that should receive the skill.
fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = default_dir() {
        dirs.push(dir);
    }
    if let Some(dir) = claude_dir() {
        dirs.push(dir);
    }
    // Do not create an OpenCode-specific copy, but keep an existing one from
    // shadowing the shared `~/.agents` copy with stale content.
    if let Some(dir) = opencode_dir()
        && dir.join("SKILL.md").is_file()
    {
        dirs.push(dir);
    }
    dirs
}

/// Discovery paths that may already contain the skill, including OpenCode's
/// explicit global skills directory.
fn detection_dirs() -> Vec<PathBuf> {
    let mut dirs = default_dirs();
    if let Some(dir) = opencode_dir()
        && !dirs.contains(&dir)
    {
        dirs.push(dir);
    }
    dirs
}

/// Write the bundled skill into `dir`. Returns `true` when the file changed.
pub fn install(dir: &Path) -> Result<bool> {
    let path = dir.join("SKILL.md");
    match std::fs::read(&path) {
        Ok(existing) if existing == SKILL.as_bytes() => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
        Err(_) => {}
    }
    crate::util::write_atomic(&path, SKILL.as_bytes())?;
    let _ = crate::util::restrict_dir(dir);
    Ok(true)
}

/// Install into the standard discovery directories. Each entry is the target
/// directory and whether its `SKILL.md` changed.
pub fn install_default() -> Result<Vec<(PathBuf, bool)>> {
    let mut results = Vec::new();
    for dir in default_dirs() {
        let changed = install(&dir)?;
        results.push((dir, changed));
    }
    Ok(results)
}

/// Standard discovery paths that currently contain a skill file.
pub fn installed_dirs() -> Vec<PathBuf> {
    detection_dirs()
        .into_iter()
        .filter(|dir| dir.join("SKILL.md").is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_the_bundled_skill_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install(dir.path()).unwrap());
        let path = dir.path().join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL);
        assert!(!install(dir.path()).unwrap());
    }

    #[test]
    fn install_replaces_stale_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "old skill").unwrap();
        assert!(install(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("SKILL.md")).unwrap(),
            SKILL
        );
    }
}
