//! Small user preferences persisted under `%APPDATA%\MiniPad`.

use crate::theme::{CustomPalette, Theme, ThemeName};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|a| PathBuf::from(a).join("MiniPad"))
}

fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.json"))
}

/// Serde default for [`Settings::theme`]: light, for compatibility.
fn default_theme() -> String {
    ThemeName::Light.as_str().to_string()
}

/// Maps a Windows locale name to one of the bundled dictionaries.
fn spell_lang_for_locale(locale: Option<&str>) -> String {
    match locale {
        Some(l) if l.split(['-', '_']).next().is_some_and(|p| p.eq_ignore_ascii_case("es")) => "es_ES".to_string(),
        _ => "en_US".to_string(),
    }
}

#[cfg(windows)]
fn system_locale_name() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if n == 0 {
        return None;
    }
    let len = (n as usize).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(not(windows))]
fn system_locale_name() -> Option<String> {
    std::env::var("LANG").or_else(|_| std::env::var("LC_ALL")).ok()
}

/// Default spelling language: Spanish when Windows is configured in Spanish,
/// otherwise American English. Both dictionaries are bundled.
fn default_spell_lang() -> String {
    spell_lang_for_locale(system_locale_name().as_deref())
}

/// Default spelling-as-you-type: off, so the editor opens silent.
fn default_spell_enabled() -> bool {
    false
}

fn default_autosave_enabled() -> bool {
    true
}

fn default_autosave_secs() -> u64 {
    60
}

/// User preferences. Missing fields fall back to their defaults so older files
/// keep loading after the format grows.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Font families the user starred, in order. The first one becomes the
    /// default for new documents.
    pub favorites: Vec<String>,
    /// Active theme name: `"Light"`, `"Dark"` or `"Custom"`.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Palette of the Custom theme. `None` means the user has not customised
    /// anything yet; the editor then seeds it from the active theme.
    #[serde(default)]
    pub custom_theme: Option<CustomPalette>,
    /// Language code used by the spell checker (`en_US`, `es_ES`, ...).
    #[serde(default = "default_spell_lang")]
    pub spell_lang: String,
    /// Whether "check spelling as you type" is enabled. Off by default so the
    /// editor opens with no red squiggles when no language pack is installed.
    #[serde(default = "default_spell_enabled")]
    pub spell_enabled: bool,
    /// Whether crash-recovery snapshots are written while there are unsaved edits.
    #[serde(default = "default_autosave_enabled")]
    pub autosave_enabled: bool,
    /// Seconds between recovery snapshots (clamped to at least 10).
    #[serde(default = "default_autosave_secs")]
    pub autosave_secs: u64,
    /// Whether the navigation pane is open.
    #[serde(default)]
    pub show_nav: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            favorites: Vec::new(),
            theme: default_theme(),
            custom_theme: None,
            spell_lang: default_spell_lang(),
            spell_enabled: default_spell_enabled(),
            autosave_enabled: default_autosave_enabled(),
            autosave_secs: default_autosave_secs(),
            show_nav: false,
        }
    }
}

impl Settings {
    /// Reads the preferences; any error (missing file, bad JSON) gives defaults.
    pub fn load() -> Settings {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes the preferences, creating the folder if needed (best effort).
    /// The write is atomic: a sibling temp file is renamed over the target so
    /// a crash mid-write cannot leave a truncated `settings.json`.
    pub fn save(&self) {
        let (Some(dir), Some(path)) = (dir(), path()) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let Ok(s) = serde_json::to_string_pretty(self) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// The persisted theme, mapping anything unknown to Light.
    pub fn theme(&self) -> Theme {
        match self.theme_name() {
            ThemeName::Dark => Theme::dark(),
            ThemeName::Custom => Theme::custom(&self.custom_theme.unwrap_or_default()),
            ThemeName::Light => Theme::light(),
        }
    }

    /// The persisted theme name, defaulting to Light.
    pub fn theme_name(&self) -> ThemeName {
        if self.theme.eq_ignore_ascii_case("Dark") {
            ThemeName::Dark
        } else if self.theme.eq_ignore_ascii_case("Custom") {
            ThemeName::Custom
        } else {
            ThemeName::Light
        }
    }

    /// Stores the active theme name.
    pub fn set_theme(&mut self, name: ThemeName) {
        self.theme = name.as_str().to_string();
    }

    /// Stores the Custom palette (used when the Custom theme is active).
    pub fn set_custom_theme(&mut self, palette: CustomPalette) {
        self.custom_theme = Some(palette);
    }

    /// Stores the preferred spelling language code.
    pub fn set_spell_lang(&mut self, code: &str) {
        self.spell_lang = code.to_string();
    }

    /// Stores whether "check spelling as you type" is enabled.
    pub fn set_spell_enabled(&mut self, on: bool) {
        self.spell_enabled = on;
    }

    /// Effective autosave interval, never below 10 seconds.
    pub fn autosave_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.autosave_secs.max(10))
    }

    /// Toggles a font family in the favorites list, preserving order.
    pub fn toggle_favorite(&mut self, name: &str) {
        if let Some(i) = self.favorites.iter().position(|f| f.eq_ignore_ascii_case(name)) {
            self.favorites.remove(i);
        } else {
            self.favorites.push(name.to_string());
        }
    }

    pub fn is_favorite(&self, name: &str) -> bool {
        self.favorites.iter().any(|f| f.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_toggle() {
        let mut s = Settings::default();
        assert!(!s.is_favorite("Georgia"));
        s.toggle_favorite("Georgia");
        s.toggle_favorite("Garamond");
        assert!(s.is_favorite("georgia"), "case-insensitive");
        assert_eq!(s.favorites, vec!["Georgia".to_string(), "Garamond".to_string()]);
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.favorites, s.favorites);
        // A toggle removes it.
        s.toggle_favorite("GEORGIA");
        assert_eq!(s.favorites, vec!["Garamond".to_string()]);
        // Unknown/missing fields load with defaults.
        let partial: Settings = serde_json::from_str("{}").unwrap();
        assert!(partial.favorites.is_empty());
        assert_eq!(partial.theme_name(), ThemeName::Light);
    }

    #[test]
    fn spell_prefs_default_off_and_follow_system_language() {
        let s = Settings::default();
        assert!(!s.spell_enabled, "check-as-you-type must default to off");
        assert_eq!(s.spell_lang, default_spell_lang());
        // Old files without the fields keep the defaults.
        let partial: Settings = serde_json::from_str("{}").unwrap();
        assert!(!partial.spell_enabled);
        assert_eq!(partial.spell_lang, default_spell_lang());
        for es in ["es-ES", "es-MX", "ES_es"] {
            assert_eq!(spell_lang_for_locale(Some(es)), "es_ES", "{es}");
        }
        for other in [None, Some("en-US"), Some("fr-FR"), Some("")] {
            assert_eq!(spell_lang_for_locale(other), "en_US", "{other:?}");
        }
    }

    #[test]
    fn theme_round_trips() {
        let mut s = Settings::default();
        assert_eq!(s.theme_name(), ThemeName::Light);
        s.set_theme(ThemeName::Dark);
        assert_eq!(s.theme, "Dark");
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme_name(), ThemeName::Dark);
    }

    #[test]
    fn custom_theme_round_trips() {
        let mut s = Settings::default();
        let mut p = CustomPalette::default();
        p.chrome_bg = eframe::egui::Color32::from_rgb(0x11, 0x22, 0x33);
        s.set_custom_theme(p);
        s.set_theme(ThemeName::Custom);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("#112233"));
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme_name(), ThemeName::Custom);
        assert_eq!(back.custom_theme.unwrap().chrome_bg, p.chrome_bg);
        assert_eq!(back.theme().name, ThemeName::Custom);
        assert_eq!(back.theme().tokens.chrome_bg, p.chrome_bg);

        // Older files (without `custom_theme`) still load as Light.
        let partial: Settings = serde_json::from_str("{}").unwrap();
        assert!(partial.custom_theme.is_none());
        assert_eq!(partial.theme_name(), ThemeName::Light);
    }
}
