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
    SunsetEmber,
    TerracottaDawn,
    OrchidDusk,
    RoseQuartz,
    HighContrastDark,
    HighContrastLight,
    GithubLight,
}

impl ThemeId {
    pub const ALL: [Self; 12] = [
        Self::TokyoNightStorm,
        Self::CatppuccinMacchiato,
        Self::KanagawaDragon,
        Self::Nord,
        Self::GruvboxMaterialDark,
        Self::SunsetEmber,
        Self::TerracottaDawn,
        Self::OrchidDusk,
        Self::RoseQuartz,
        Self::HighContrastDark,
        Self::HighContrastLight,
        Self::GithubLight,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::TokyoNightStorm => "tokyo-night-storm",
            Self::CatppuccinMacchiato => "catppuccin-macchiato",
            Self::KanagawaDragon => "kanagawa-dragon",
            Self::Nord => "nord",
            Self::GruvboxMaterialDark => "gruvbox-material-dark",
            Self::SunsetEmber => "sunset-ember",
            Self::TerracottaDawn => "terracotta-dawn",
            Self::OrchidDusk => "orchid-dusk",
            Self::RoseQuartz => "rose-quartz",
            Self::HighContrastDark => "high-contrast-dark",
            Self::HighContrastLight => "high-contrast-light",
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
            Self::SunsetEmber => "Sunset Ember",
            Self::TerracottaDawn => "Terracotta Dawn",
            Self::OrchidDusk => "Orchid Dusk",
            Self::RoseQuartz => "Rose Quartz",
            Self::HighContrastDark => "High Contrast Dark",
            Self::HighContrastLight => "High Contrast Light",
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
            Self::SunsetEmber => ThemePalette {
                id: self,
                label: self.label(),
                background: "#1f1411",
                surface: "#2b1d19",
                surface_alt: "#3a2822",
                text: "#f6ddc7",
                muted: "#d0a885",
                accent: "#ff9b54",
                mention: "#ff6f7d",
                unread: "#c8e26a",
            },
            Self::TerracottaDawn => ThemePalette {
                id: self,
                label: self.label(),
                background: "#fff3e8",
                surface: "#fffaf5",
                surface_alt: "#f4e3d5",
                text: "#4a3128",
                muted: "#9b6f5d",
                accent: "#c96b3b",
                mention: "#b42318",
                unread: "#317d4a",
            },
            Self::OrchidDusk => ThemePalette {
                id: self,
                label: self.label(),
                background: "#16131f",
                surface: "#221c30",
                surface_alt: "#2f2742",
                text: "#efe8ff",
                muted: "#aa9bcf",
                accent: "#b38cff",
                mention: "#ff96d8",
                unread: "#7fe6b1",
            },
            Self::RoseQuartz => ThemePalette {
                id: self,
                label: self.label(),
                background: "#fff1f6",
                surface: "#ffffff",
                surface_alt: "#f5dce6",
                text: "#492534",
                muted: "#9d6b7d",
                accent: "#d9487f",
                mention: "#9c2f5d",
                unread: "#2f7d57",
            },
            Self::HighContrastDark => ThemePalette {
                id: self,
                label: self.label(),
                background: "#000000",
                surface: "#111111",
                surface_alt: "#1b1b1b",
                text: "#ffffff",
                muted: "#d0d0d0",
                accent: "#00e5ff",
                mention: "#ff5c5c",
                unread: "#b8ff00",
            },
            Self::HighContrastLight => ThemePalette {
                id: self,
                label: self.label(),
                background: "#ffffff",
                surface: "#f2f2f2",
                surface_alt: "#e4e4e4",
                text: "#111111",
                muted: "#303030",
                accent: "#0047ff",
                mention: "#c21807",
                unread: "#006b2e",
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
    background: {surface};
    color: {text};
    font-family: \"IBM Plex Sans\", \"Noto Sans JP\", Cantarell, sans-serif;
}}
.app-root {{
    background: {surface};
}}
label {{
    color: {text};
}}
.icon-rail {{
    background: {surface};
    border-radius: 0;
    padding: 14px 10px;
}}
.deck-column {{
    background: {surface};
    border-radius: 0;
    padding: 14px;
}}
.timeline-column {{
    padding: 0;
    border-radius: 0;
}}
.timeline-head {{
    padding: 14px 14px 0 14px;
}}
.timeline-filterbar {{
    padding-bottom: 2px;
}}
.settings-column {{
    background: {surface_alt};
}}
.card {{
    background: shade({surface_alt}, 1.04);
    border-radius: 16px;
    border: 1px solid alpha({muted}, 0.28);
    padding: 12px;
}}
.card {{
    margin: 6px 0;
}}
.card-active, .thread-root {{
    border: 2px solid {accent};
}}
.timeline-list {{
    background: transparent;
}}
.timeline-tile {{
    background: transparent;
    border-radius: 0;
    border-bottom: 1px solid alpha({muted}, 0.24);
    padding: 14px;
    margin: 0;
}}
.timeline-tile:hover {{
    background: alpha({accent}, 0.08);
}}
.timeline-tile-active {{
    background: alpha({accent}, 0.12);
    border-left: 4px solid {accent};
}}
.timeline-tile-fresh {{
    background: alpha({accent}, 0.18);
    border-left: 4px solid alpha({accent}, 0.88);
}}
.timeline-empty {{
    min-height: 200px;
}}
.author {{
    color: {text};
    font-weight: 700;
}}
.timestamp {{
    color: {muted};
}}
.avatar-shell {{
    min-width: 44px;
    min-height: 44px;
    background: transparent;
    border-radius: 999px;
}}
.avatar-shell-media {{
    border: none;
    background: transparent;
}}
.avatar-fallback {{
    background: alpha({accent}, 0.18);
    border-radius: 999px;
    border: 1px solid alpha({muted}, 0.24);
}}
.avatar-media {{
    border-radius: 999px;
}}
.avatar-initials {{
    color: {accent};
    font-weight: 800;
}}
.brand-mark {{
    min-height: 48px;
}}
.pill {{
    background: alpha({accent}, 0.16);
    color: {accent};
    border-radius: 999px;
    padding: 4px 10px;
    font-weight: 700;
}}
.meta {{
    color: {muted};
}}
.slack-rich-paragraph,
.slack-rich-quote-text,
.slack-rich-list-text,
.slack-code-text {{
    color: {text};
    line-height: 1.2;
}}
.slack-rich-quote {{
    border-left: 3px solid alpha({muted}, 0.45);
    padding-left: 10px;
}}
.slack-rich-list-bullet {{
    color: {muted};
    min-width: 24px;
    font-weight: 700;
}}
.slack-code-block {{
    background: #f6f3ed;
    border: 1px solid #ddd4c7;
    border-radius: 8px;
    padding: 10px 12px;
}}
.slack-code-text,
.slack-code-view,
.slack-code-view text {{
    color: #2d2d29;
    background: transparent;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.slack-rich-table-wrap {{
    background: #fffdfa;
    border: 1px solid #ddd4c7;
    border-radius: 10px;
}}
.slack-rich-table-header,
.slack-rich-table-cell {{
    padding: 8px 10px;
    border-right: 1px solid #e6ddd0;
    border-bottom: 1px solid #e6ddd0;
}}
.slack-rich-table-header {{
    background: #f3eee4;
}}
.slack-rich-table-cell {{
    background: #fffdfa;
}}
.slack-rich-table-edge-right {{
    border-right: none;
}}
.slack-rich-table-edge-bottom {{
    border-bottom: none;
}}
.slack-rich-table-corner-top-left {{
    border-top-left-radius: 10px;
}}
.slack-rich-table-corner-top-right {{
    border-top-right-radius: 10px;
}}
.slack-rich-table-corner-bottom-left {{
    border-bottom-left-radius: 10px;
}}
.slack-rich-table-corner-bottom-right {{
    border-bottom-right-radius: 10px;
}}
.slack-rich-table-text {{
    color: #2d2d29;
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
button.nav-button {{
    background: transparent;
    color: {muted};
    border-radius: 16px;
    min-width: 48px;
    min-height: 48px;
    padding: 10px;
}}
button.nav-button:hover {{
    background: alpha({accent}, 0.12);
    color: {text};
}}
button.nav-button.active {{
    background: {accent};
    color: {background};
}}
button:hover {{
    background: alpha({accent}, 0.10);
}}
button.reply-button {{
    background: transparent;
    color: {muted};
    border-radius: 10px;
    min-width: 28px;
    min-height: 28px;
    padding: 4px;
}}
button.reply-button:hover {{
    background: alpha({accent}, 0.12);
}}
.reply-count {{
    color: {muted};
    font-weight: 700;
}}
button.close-button {{
    background: transparent;
    color: {muted};
    border-radius: 12px;
    min-width: 36px;
    min-height: 36px;
    padding: 8px;
}}
button.close-button:hover {{
    background: alpha({accent}, 0.12);
}}
textview {{
    background: {surface_alt};
    color: {text};
    border-radius: 14px;
}}
button.suggested-action {{
    background: {accent};
    color: {background};
    border-radius: 12px;
}}
button.suggested-action:hover {{
    background: shade({accent}, 1.08);
}}
entry, searchentry, combobox {{
    background: {surface_alt};
    color: {text};
    border-radius: 12px;
}}
entry:hover, searchentry:hover, combobox:hover {{
    background: alpha({accent}, 0.10);
    border-color: alpha({accent}, 0.55);
}}
scrolledwindow {{
    background: transparent;
}}
.thread-message {{
    margin: 0;
    border-radius: 0;
    border-bottom: 1px solid alpha({muted}, 0.24);
}}
.attachment-strip {{
    margin-top: 2px;
}}
.reaction-bar {{
    margin-top: 2px;
}}
button.reaction-chip {{
    background: alpha({accent}, 0.08);
    color: {text};
    border-radius: 999px;
    padding: 3px 10px;
    min-height: 28px;
}}
button.reaction-chip:hover {{
    background: alpha({accent}, 0.14);
}}
button.reaction-chip.active {{
    background: alpha({accent}, 0.22);
    color: {accent};
}}
button.quick-reaction-chip {{
    padding: 2px 10px;
}}
menubutton.reaction-add-button > button {{
    background: alpha({accent}, 0.05);
    color: {muted};
    border-radius: 999px;
    padding: 3px 10px;
    min-height: 28px;
}}
menubutton.reaction-add-button > button:hover {{
    background: alpha({accent}, 0.14);
    color: {text};
}}
.reaction-popover {{
    padding: 4px;
}}
.reaction-entry {{
    min-width: 144px;
}}
.shared-preview-strip {{
    margin-top: 2px;
}}
.shared-preview-card {{
    background: alpha({accent}, 0.04);
    border: 1px solid alpha({muted}, 0.16);
    border-radius: 12px;
    padding: 8px 10px;
}}
.shared-preview-body {{
    color: {text};
}}
.attachment-card {{
    background: alpha({accent}, 0.06);
    border: 1px solid alpha({muted}, 0.18);
    border-radius: 14px;
    padding: 8px;
}}
.attachment-image {{
    border-radius: 14px;
}}
.attachment-label {{
    color: {text};
    font-weight: 600;
}}
.office-scene {{
    background: transparent;
}}
button.office-desk {{
    background: transparent;
    color: {text};
    border: none;
    border-radius: 0;
    padding: 0;
    min-width: 240px;
    min-height: 238px;
}}
button.office-desk:hover {{
    background: alpha({accent}, 0.06);
}}
.office-bubble {{
    background: #ffffff;
    border-radius: 18px;
    padding: 10px 12px;
    border: 3px solid #b8b8b1;
    min-height: 92px;
}}
.office-bubble-tail {{
    background: transparent;
}}
.office-nameplate {{
    background: transparent;
    border-radius: 0;
    padding: 0;
    border: none;
}}
.office-channel {{
    color: #2d2d29;
    font-weight: 700;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.office-bubble-body {{
    color: #2d2d29;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
    line-height: 1.15;
}}
.office-avatar-shell {{
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
}}
.office-avatar {{
    margin-top: 0;
    border-radius: 999px;
}}
.office-pixel-avatar {{
    background: transparent;
}}
.office-speaker {{
    color: {background};
    font-weight: 700;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.office-signal {{
    color: {mention};
    font-weight: 900;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.office-activity-badge {{
    background: transparent;
    color: shade({muted}, 0.72);
    border-radius: 0;
    padding: 0;
    font-weight: 700;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.office-desk-top {{
    background: #8b6445;
    border: 3px solid #5f402c;
    border-radius: 0;
}}
.office-desk-trim {{
    background: #6f4a33;
}}
.office-desk-modesty {{
    background: #79523a;
}}
.office-desk-leg {{
    background: #5b3d29;
}}
.office-desk-drawer {{
    background: #a57855;
    border: 2px solid #6d4a32;
}}
.office-monitor-shell {{
    background: #2d3238;
    border: 3px solid #181c20;
}}
.office-monitor-screen {{
    background: #7fd1ff;
    border: 2px solid #2d5c74;
}}
.office-monitor-stand {{
    background: #444a53;
}}
.office-monitor-base {{
    background: #2b3138;
}}
.office-desk-hot .office-monitor-screen {{
    background: #ff9a66;
}}
.office-desk-warm .office-monitor-screen {{
    background: #9ad8ff;
}}
.office-keyboard-shell {{
    background: #e1d4bf;
    border: 2px solid #b8a890;
}}
.office-key {{
    background: #c8baa3;
}}
.office-chair-back {{
    background: #5d6d83;
    border: 3px solid #38414d;
}}
.office-chair-seat {{
    background: #6f829c;
    border: 3px solid #404b59;
}}
.office-chair-stem {{
    background: #3a4048;
}}
.office-chair-base {{
    background: #2a3038;
}}
.office-chair-wheel {{
    background: #20262d;
}}
.office-mug-body {{
    background: #d65e48;
    border: 2px solid #964031;
}}
.office-mug-handle {{
    background: #efc5b9;
    border: 2px solid #964031;
}}
.office-break-mat {{
    background: #d9c5a0;
    border: 3px solid #b3976e;
    border-radius: 18px;
}}
.office-sofa-back {{
    background: #7f9a82;
    border: 3px solid #576c59;
    border-radius: 12px 12px 4px 4px;
}}
.office-sofa-arm {{
    background: #6c866f;
    border: 3px solid #506253;
    border-radius: 10px;
}}
.office-sofa-seat {{
    background: #91ab94;
    border: 3px solid #5f745f;
    border-radius: 8px;
}}
.office-sofa-cushion {{
    background: #a6bea7;
    border: 2px solid #718873;
    border-radius: 8px;
}}
.office-sofa-leg {{
    background: #5b3d29;
}}
.office-table-top {{
    background: #b7895f;
    border: 3px solid #7a5638;
    border-radius: 10px;
}}
.office-table-leg {{
    background: #7a5638;
}}
.office-table-shelf {{
    background: #9b6f49;
    border-radius: 6px;
}}
.office-table-book {{
    background: #6f8fb6;
    border: 2px solid #4e6685;
    border-radius: 4px;
}}
.office-table-mug {{
    background: #efe4d8;
    border: 2px solid #8f6d53;
    border-radius: 6px;
}}
.office-vending-shell {{
    background: #f2eee5;
    border: 3px solid #b9b1a2;
    border-radius: 0;
}}
.office-vending-window {{
    background: #a7d9f2;
    border: 2px solid #6e96ae;
}}
.office-vending-panel {{
    background: #d3cabd;
    border: 2px solid #a59b8d;
}}
.office-vending-slot {{
    background: #777066;
}}
.office-vending-button {{
    background: #6ca6c1;
}}
.office-whiteboard-frame {{
    background: #c4a27f;
    border: 3px solid #8b6b4f;
}}
.office-whiteboard-panel {{
    background: #f8f7f2;
    border: 2px solid #d8d4c8;
}}
.office-note-yellow {{
    background: #f2dc6b;
}}
.office-note-blue {{
    background: #8fcbef;
}}
.office-note-red {{
    background: #e98e7f;
}}
.office-bookshelf-shell {{
    background: #7d5e44;
    border: 3px solid #5e442f;
}}
.office-bookshelf-plank {{
    background: #5d432f;
}}
.office-book-green {{
    background: #83a15a;
}}
.office-book-blue {{
    background: #6d8fb8;
}}
.office-book-red {{
    background: #b86857;
}}
.office-book-yellow {{
    background: #d8bf67;
}}
.office-planter-pot {{
    background: #956643;
    border: 3px solid #6b462d;
}}
.office-planter-soil {{
    background: #4a311f;
}}
.office-planter-leaf-a {{
    background: #679d59;
}}
.office-planter-leaf-b {{
    background: #7db56c;
}}
.office-planter-leaf-c {{
    background: #4f8247;
}}
.office-zone-label {{
    color: #5e5240;
    font-weight: 700;
    font-family: \"IBM Plex Mono\", \"Noto Sans Mono\", monospace;
}}
.office-empty-card {{
    background: {surface};
    border-radius: 0;
    border: 3px solid shade({muted}, 0.80);
    padding: 22px;
    min-width: 280px;
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
