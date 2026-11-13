//! Built-in dashboard themes and the selected-theme contract.

use serde::Serialize;

pub const DEFAULT_THEME: &str = "nocturne";

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
];

pub fn default_theme() -> &'static str {
    DEFAULT_THEME
}

/// Return the current id for a theme, accepting names from older Orqella
/// releases so existing `config.toml` files keep working after a rename.
pub fn canonical(id: &str) -> Option<&'static str> {
    match id {
        "nocturne" | "catppuccin-mocha" => Some("nocturne"),
        "signal" | "tokyo-night" => Some("signal"),
        "daybreak" | "catppuccin-latte" => Some("daybreak"),
        "foundry" | "vscode-dark-modern" => Some("foundry"),
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
        assert_eq!(color_scheme("daybreak"), "light");
        assert_eq!(asset_path("signal"), "themes/signal.css");
    }

    #[test]
    fn legacy_ids_are_canonicalized() {
        assert_eq!(normalize("catppuccin-mocha"), "nocturne");
        assert_eq!(normalize("tokyo-night"), "signal");
        assert_eq!(normalize("catppuccin-latte"), "daybreak");
        assert_eq!(normalize("vscode-dark-modern"), "foundry");
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
    fn canonicalization_covers_legacy_aliases() {
        assert_eq!(canonical("signal"), Some("signal"));
        assert_eq!(canonical("tokyo-night"), Some("signal"));
        assert_eq!(canonical("unknown"), None);
        assert_eq!(normalize("catppuccin-mocha"), "nocturne");
    }
}
