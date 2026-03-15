use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeId {
    TokyoNightStorm,
    CatppuccinMacchiato,
    KanagawaDragon,
    Nord,
    GruvboxMaterialDark,
    GithubLight,
}

impl ThemeId {
    pub const ALL: [Self; 6] = [
        Self::TokyoNightStorm,
        Self::CatppuccinMacchiato,
        Self::KanagawaDragon,
        Self::Nord,
        Self::GruvboxMaterialDark,
        Self::GithubLight,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::TokyoNightStorm => "tokyo-night-storm",
            Self::CatppuccinMacchiato => "catppuccin-macchiato",
            Self::KanagawaDragon => "kanagawa-dragon",
            Self::Nord => "nord",
            Self::GruvboxMaterialDark => "gruvbox-material-dark",
            Self::GithubLight => "github-light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TokyoNightStorm => "Tokyo Night Storm",
            Self::CatppuccinMacchiato => "Catppuccin Macchiato",
            Self::KanagawaDragon => "Kanagawa Dragon",
            Self::Nord => "Nord",
            Self::GruvboxMaterialDark => "Gruvbox Material Dark",
            Self::GithubLight => "GitHub Light",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.slug() == slug)
    }

    pub fn palette(self) -> ThemePalette {
        match self {
            Self::TokyoNightStorm => ThemePalette {
                id: self,
                label: self.label(),
                background: "#16161e",
                surface: "#1f2335",
                surface_alt: "#24283b",
                text: "#c0caf5",
                muted: "#7a88cf",
                accent: "#7aa2f7",
                mention: "#ff9e64",
                unread: "#9ece6a",
            },
            Self::CatppuccinMacchiato => ThemePalette {
                id: self,
                label: self.label(),
                background: "#24273a",
                surface: "#1e2030",
                surface_alt: "#363a4f",
                text: "#cad3f5",
                muted: "#8087a2",
                accent: "#8aadf4",
                mention: "#f5a97f",
                unread: "#a6da95",
            },
            Self::KanagawaDragon => ThemePalette {
                id: self,
                label: self.label(),
                background: "#181616",
                surface: "#1d1c19",
                surface_alt: "#2d4f67",
                text: "#c5c9c5",
                muted: "#8a9a7b",
                accent: "#8ba4b0",
                mention: "#c4746e",
                unread: "#87a987",
            },
            Self::Nord => ThemePalette {
                id: self,
                label: self.label(),
                background: "#2e3440",
                surface: "#3b4252",
                surface_alt: "#434c5e",
                text: "#eceff4",
                muted: "#81a1c1",
                accent: "#88c0d0",
                mention: "#d08770",
                unread: "#a3be8c",
            },
            Self::GruvboxMaterialDark => ThemePalette {
                id: self,
                label: self.label(),
                background: "#282828",
                surface: "#32302f",
                surface_alt: "#3c3836",
                text: "#d4be98",
                muted: "#a89984",
                accent: "#7daea3",
                mention: "#e78a4e",
                unread: "#a9b665",
            },
            Self::GithubLight => ThemePalette {
                id: self,
                label: self.label(),
                background: "#f6f8fa",
                surface: "#ffffff",
                surface_alt: "#eaedf0",
                text: "#24292f",
                muted: "#57606a",
                accent: "#0969da",
                mention: "#bc4c00",
                unread: "#1a7f37",
            },
        }
    }

    pub fn css(self) -> String {
        let palette = self.palette();
        format!(
            "
window {{
    background: {background};
    color: {text};
}}
label {{
    color: {text};
}}
.rail, .drawer, .card {{
    background: {surface};
    border-radius: 14px;
    padding: 12px;
}}
.card {{
    margin: 8px 0;
}}
.meta {{
    color: {muted};
}}
.accent {{
    color: {accent};
}}
.mention {{
    color: {mention};
}}
.unread {{
    color: {unread};
}}
textview {{
    background: {surface_alt};
    color: {text};
}}
button.suggested-action {{
    background: {accent};
    color: {background};
}}
entry, combobox, scrolledwindow {{
    background: {surface_alt};
}}
",
            background = palette.background,
            surface = palette.surface,
            surface_alt = palette.surface_alt,
            text = palette.text,
            muted = palette.muted,
            accent = palette.accent,
            mention = palette.mention,
            unread = palette.unread,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    pub id: ThemeId,
    pub label: &'static str,
    pub background: &'static str,
    pub surface: &'static str,
    pub surface_alt: &'static str,
    pub text: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
    pub mention: &'static str,
    pub unread: &'static str,
}

pub fn builtin_themes() -> BTreeMap<ThemeId, ThemePalette> {
    ThemeId::ALL
        .into_iter()
        .map(|theme| (theme, theme.palette()))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::theme::{ThemeId, builtin_themes};

    #[test]
    fn all_theme_ids_are_exposed() {
        let themes = builtin_themes();

        assert_eq!(themes.len(), ThemeId::ALL.len());
        assert_eq!(
            ThemeId::from_slug("tokyo-night-storm"),
            Some(ThemeId::TokyoNightStorm)
        );
    }
}
