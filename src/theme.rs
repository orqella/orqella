//! Built-in dashboard themes, custom themes, and the selected-theme contract.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_THEME: &str = "nocturne";
const CUSTOM_THEMES_DIR: &str = "themes";
const MAX_NAME_CHARS: usize = 48;
const MAX_DESCRIPTION_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ThemeSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub color_scheme: &'static str,
}

pub const THEMES: &[ThemeSpec] = &[
    ThemeSpec {
        id: "nocturne",
        name: "Nocturne",
        description: "Orqella's default: deep indigo surfaces, soft text, and a blue-lavender accent.",
        color_scheme: "dark",
    },
    ThemeSpec {
        id: "signal",
        name: "Signal",
        description: "An electric-blue night with violet highlights and a high-contrast edge.",
        color_scheme: "dark",
    },
    ThemeSpec {
        id: "daybreak",
        name: "Daybreak",
        description: "Cool, light neutral surfaces for daylight work.",
        color_scheme: "light",
    },
    ThemeSpec {
        id: "foundry",
        name: "Foundry",
        description: "Neutral graphite chrome with VS Code's default dark blue accent.",
        color_scheme: "dark",
    },
    ThemeSpec {
        id: "cyber",
        name: "Cyber",
        description: "A calm phosphor-green circuit night with pulsing traces, faint rain, and CRT scanlines.",
        color_scheme: "dark",
    },
    ThemeSpec {
        id: "btop",
        name: "btop",
        description: "The btop++ terminal look: black panes, monospace type, square box borders, embedded titles, and green-to-red meters.",
        color_scheme: "dark",
    },
    ThemeSpec {
        id: "night-city",
        name: "Night City",
        description: "btop++'s black-and-green palette and monospace type over Foundry's clean, geometric chrome.",
        color_scheme: "dark",
    },
];

pub fn default_theme() -> &'static str {
    DEFAULT_THEME
}

/// Return the current id for a theme, accepting names from older Orqella
/// releases so existing `config.toml` files keep working after a rename.
/// The first-party Catppuccin themes were retired; their ids remain aliases
/// for the Nocturne and Daybreak palettes they were originally built from.
pub fn canonical(id: &str) -> Option<&'static str> {
    match id {
        "nocturne" | "catppuccin-mocha" => Some("nocturne"),
        "signal" | "tokyo-night" => Some("signal"),
        "daybreak" | "catppuccin-latte" => Some("daybreak"),
        "foundry" | "vscode-dark-modern" => Some("foundry"),
        "cyber" => Some("cyber"),
        "btop" => Some("btop"),
        "night-city" => Some("night-city"),
        _ => None,
    }
}

pub fn normalize(id: &str) -> &'static str {
    canonical(id).unwrap_or(DEFAULT_THEME)
}

pub fn is_known(id: &str) -> bool {
    canonical(id).is_some()
}

pub fn color_scheme(id: &str) -> &'static str {
    let id = normalize(id);
    THEMES
        .iter()
        .find(|theme| theme.id == id)
        .map(|theme| theme.color_scheme)
        .unwrap_or("dark")
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color_scheme: String,
    pub custom: bool,
}

impl From<&ThemeSpec> for ThemeOption {
    fn from(theme: &ThemeSpec) -> Self {
        Self {
            id: theme.id.to_string(),
            name: theme.name.to_string(),
            description: theme.description.to_string(),
            color_scheme: theme.color_scheme.to_string(),
            custom: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CustomThemeMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color_scheme: String,
    #[serde(default)]
    created_at: String,
}

fn custom_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(CUSTOM_THEMES_DIR)
}

fn valid_custom_id(id: &str) -> bool {
    id.starts_with("custom-")
        && id.len() <= 80
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn custom_meta(config_dir: &Path, id: &str) -> Option<CustomThemeMeta> {
    let path = custom_dir(config_dir).join(format!("{id}.json"));
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn custom_option(config_dir: &Path, id: &str) -> Option<ThemeOption> {
    if !valid_custom_id(id) || custom_css(config_dir, id).is_none() {
        return None;
    }
    let meta = custom_meta(config_dir, id).unwrap_or_default();
    let name = meta.name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = if name.is_empty() {
        id.to_string()
    } else {
        name
    };
    let scheme = meta.color_scheme.trim().to_ascii_lowercase();
    let color_scheme = if matches!(scheme.as_str(), "light" | "dark") {
        scheme
    } else {
        "dark".into()
    };
    Some(ThemeOption {
        id: id.to_string(),
        name,
        description: meta
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        color_scheme,
        custom: true,
    })
}

/// Built-in themes followed by installed custom themes.
pub fn catalog(config_dir: &Path) -> Vec<ThemeOption> {
    let mut themes: Vec<ThemeOption> = THEMES.iter().map(ThemeOption::from).collect();
    let mut custom = Vec::new();
    if let Ok(entries) = std::fs::read_dir(custom_dir(config_dir)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("css") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if let Some(option) = custom_option(config_dir, id) {
                custom.push(option);
            }
        }
    }
    custom.sort_by_key(|theme| theme.name.to_lowercase());
    themes.extend(custom);
    themes
}

pub fn custom_css(config_dir: &Path, id: &str) -> Option<String> {
    if !valid_custom_id(id) {
        return None;
    }
    let path = custom_dir(config_dir).join(format!("{id}.css"));
    std::fs::read_to_string(path)
        .ok()
        .filter(|css| !css.trim().is_empty())
}

pub fn selected_color_scheme(config_dir: &Path, id: &str) -> String {
    if let Some(scheme) = canonical(id).map(color_scheme) {
        return scheme.to_string();
    }
    custom_option(config_dir, id)
        .map(|theme| theme.color_scheme)
        .unwrap_or_else(|| color_scheme(DEFAULT_THEME).to_string())
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
        if slug.len() >= 32 {
            break;
        }
    }
    slug.trim_matches('-').to_string()
}

pub fn suggest_name(prompt: &str) -> String {
    let words = prompt
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let mut name = words
        .chars()
        .filter(|character| !matches!(character, '\n' | '\r'))
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim_matches(|character: char| character.is_ascii_punctuation())
        .trim()
        .to_string();
    if name.is_empty() {
        return "Custom theme".into();
    }
    if let Some(first) = name.get(0..1) {
        name.replace_range(0..1, &first.to_ascii_uppercase());
    }
    name
}

pub fn new_custom_id(name: &str) -> String {
    let slug = slugify(name);
    let slug = if slug.is_empty() { "theme" } else { &slug };
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("custom-{slug}-{}", &suffix[..8])
}

fn clean_description(value: &str, fallback: &str) -> String {
    let value = if value.trim().is_empty() {
        fallback
    } else {
        value
    };
    value.trim().chars().take(MAX_DESCRIPTION_CHARS).collect()
}

pub fn install_custom(
    config_dir: &Path,
    id: &str,
    name: &str,
    description: &str,
    color_scheme: &str,
    css: &str,
) -> Result<()> {
    if !valid_custom_id(id) {
        bail!("invalid custom theme id");
    }
    let name = name.trim();
    if name.is_empty() {
        bail!("theme name is required");
    }
    if !matches!(color_scheme, "light" | "dark") {
        bail!("theme color scheme must be light or dark");
    }
    if css.trim().is_empty() {
        bail!("theme CSS is empty");
    }
    let dir = custom_dir(config_dir);
    std::fs::create_dir_all(&dir)?;
    let css_path = dir.join(format!("{id}.css"));
    if css_path.exists() {
        bail!("theme id already exists");
    }
    let meta = CustomThemeMeta {
        name: name.chars().take(MAX_NAME_CHARS).collect(),
        description: clean_description(description, ""),
        color_scheme: color_scheme.to_string(),
        created_at: crate::util::now_iso(),
    };
    let raw = serde_json::to_vec_pretty(&meta)?;
    crate::util::write_atomic(&dir.join(format!("{id}.json")), &raw)?;
    crate::util::write_atomic(&css_path, css.as_bytes())?;
    Ok(())
}

/// The embedded CSS asset for a theme. The default is kept at `theme.css` so
/// the historic route remains available; named themes live under `themes/`.
pub fn asset_path(id: &str) -> String {
    let id = normalize(id);
    if id == DEFAULT_THEME {
        "theme.css".into()
    } else {
        format!("themes/{id}.css")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_known() {
        assert!(is_known(DEFAULT_THEME));
        assert_eq!(color_scheme(DEFAULT_THEME), "dark");
        assert_eq!(asset_path(DEFAULT_THEME), "theme.css");
    }

    #[test]
    fn named_themes_are_known() {
        assert!(is_known("signal"));
        assert!(is_known("daybreak"));
        assert!(is_known("foundry"));
        assert!(is_known("cyber"));
        assert!(is_known("btop"));
        assert!(is_known("night-city"));
        assert_eq!(color_scheme("daybreak"), "light");
        assert_eq!(color_scheme("cyber"), "dark");
        assert_eq!(color_scheme("btop"), "dark");
        assert_eq!(color_scheme("night-city"), "dark");
        assert_eq!(asset_path("signal"), "themes/signal.css");
        assert_eq!(asset_path("cyber"), "themes/cyber.css");
        assert_eq!(asset_path("btop"), "themes/btop.css");
        assert_eq!(asset_path("night-city"), "themes/night-city.css");
    }

    #[test]
    fn legacy_ids_are_canonicalized() {
        assert_eq!(normalize("tokyo-night"), "signal");
        assert_eq!(normalize("vscode-dark-modern"), "foundry");
    }

    #[test]
    fn retired_catppuccin_ids_fall_back_to_their_origin_themes() {
        assert_eq!(normalize("catppuccin-mocha"), "nocturne");
        assert_eq!(normalize("catppuccin-latte"), "daybreak");
        assert_eq!(color_scheme("catppuccin-latte"), "light");
        assert_eq!(asset_path("catppuccin-mocha"), "theme.css");
        assert_eq!(asset_path("catppuccin-latte"), "themes/daybreak.css");
    }

    #[test]
    fn unknown_theme_is_rejected() {
        assert!(!is_known("does-not-exist"));
        assert_eq!(normalize("does-not-exist"), DEFAULT_THEME);
    }

    #[test]
    fn every_theme_has_a_shipped_stylesheet_and_unique_id() {
        let mut ids = std::collections::HashSet::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        for theme in THEMES {
            assert!(ids.insert(theme.id), "duplicate theme id {}", theme.id);
            assert!(!theme.name.trim().is_empty());
            assert!(!theme.description.trim().is_empty());
            assert!(matches!(theme.color_scheme, "dark" | "light"));
            let path = root.join(asset_path(theme.id));
            assert!(path.is_file(), "{} is missing", path.display());
            let css = std::fs::read_to_string(&path).unwrap();
            assert!(
                css.contains(":root"),
                "{} has no root tokens",
                path.display()
            );
        }
        assert_eq!(asset_path(DEFAULT_THEME), "theme.css");
    }

    #[test]
    fn cyber_theme_freezes_ambient_motion_for_the_desktop_shell() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let css = std::fs::read_to_string(root.join("themes/cyber.css")).unwrap();
        assert!(css.contains("html[data-orqella-motion=\"reduce\"]::after"));
        assert!(css.contains("animation: none;"));
    }

    #[test]
    fn canonicalization_covers_legacy_aliases() {
        assert_eq!(canonical("signal"), Some("signal"));
        assert_eq!(canonical("tokyo-night"), Some("signal"));
        assert_eq!(canonical("unknown"), None);
        assert_eq!(canonical("catppuccin-mocha"), Some("nocturne"));
        assert_eq!(canonical("catppuccin-latte"), Some("daybreak"));
        assert_eq!(canonical("night-city"), Some("night-city"));
    }

    #[test]
    fn custom_themes_install_and_appear_in_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let id = new_custom_id("Moonlit Garden");
        assert!(id.starts_with("custom-moonlit-garden-"));
        install_custom(
            dir.path(),
            &id,
            "Moonlit Garden",
            "A dark garden palette.",
            "dark",
            ":root { --bg: #101018; }",
        )
        .unwrap();
        let themes = catalog(dir.path());
        let custom = themes
            .iter()
            .find(|theme| theme.id == id)
            .expect("custom theme");
        assert_eq!(custom.name, "Moonlit Garden");
        assert_eq!(custom.description, "A dark garden palette.");
        assert!(custom.custom);
        assert!(custom_css(dir.path(), &id).is_some());
        assert_eq!(selected_color_scheme(dir.path(), &id), "dark");
        assert!(custom_css(dir.path(), "custom-missing").is_none());
        assert!(custom_css(dir.path(), "custom-../escape").is_none());
    }

    #[test]
    fn generated_names_are_short_and_presentable() {
        assert_eq!(suggest_name("moonlit forest"), "Moonlit forest");
        assert_eq!(suggest_name("   "), "Custom theme");
    }
}
