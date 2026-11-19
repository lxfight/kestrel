//! Persisted user settings, custom keybindings, and the last editor session.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font_size: f32,
    pub terminal_font_size: f32,
    pub ui_zoom: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub show_minimap: bool,
    pub show_line_numbers: bool,
    pub show_indent_guides: bool,
    pub show_whitespace: bool,
    pub show_sticky_scroll: bool,
    pub show_git_gutter: bool,
    pub autocomplete: bool,
    pub word_wrap: bool,
    pub scroll_past_end: bool,
    pub show_hidden_files: bool,
    pub autosave: bool,
    pub terminal_height: f32,
    pub sidebar_width: f32,
    pub outline_height: f32,
    pub open_editors_height: f32,
    pub git_history_height: f32,
    pub recent_folders: Vec<String>,
    pub last_folder: Option<String>,
    pub keybindings: HashMap<String, String>,
    pub disabled_plugins: Vec<String>,
    pub plugin_config: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            font_size: 14.0,
            terminal_font_size: 13.0,
            ui_zoom: 1.0,
            line_height: 1.5,
            tab_size: 4,
            insert_spaces: true,
            show_minimap: true,
            show_line_numbers: true,
            show_indent_guides: true,
            show_whitespace: false,
            show_sticky_scroll: true,
            show_git_gutter: true,
            autocomplete: true,
            word_wrap: false,
            scroll_past_end: true,
            show_hidden_files: false,
            autosave: true,
            terminal_height: 230.0,
            sidebar_width: 268.0,
            outline_height: 170.0,
            open_editors_height: 150.0,
            git_history_height: 210.0,
            recent_folders: Vec::new(),
            last_folder: None,
            keybindings: HashMap::new(),
            disabled_plugins: Vec::new(),
            plugin_config: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn config_dir() -> PathBuf {
        if cfg!(windows) {
            let base = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            base.join("Kestrel")
        } else if cfg!(target_os = "macos") {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            home.join("Library")
                .join("Application Support")
                .join("Kestrel")
        } else {
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
                PathBuf::from(xdg).join("kestrel")
            } else if let Some(home) = std::env::var_os("HOME") {
                PathBuf::from(home).join(".config").join("kestrel")
            } else {
                std::env::temp_dir().join("kestrel")
            }
        }
    }

    fn settings_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    pub fn keybindings_path() -> PathBuf {
        Self::config_dir().join("keybindings.json")
    }

    fn session_path() -> PathBuf {
        Self::config_dir().join("session.json")
    }

    pub fn load() -> Self {
        let mut settings: Self = std::fs::read_to_string(Self::settings_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if let Ok(kb_text) = std::fs::read_to_string(Self::keybindings_path())
            && let Ok(kb_map) = serde_json::from_str::<HashMap<String, String>>(&kb_text)
        {
            for (k, v) in kb_map {
                settings.keybindings.insert(k, v);
            }
        }
        settings
    }

    pub fn save(&self) {
        let dir = Self::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::settings_path(), text);
        }
        if !self.keybindings.is_empty()
            && let Ok(kb) = serde_json::to_string_pretty(&self.keybindings)
        {
            let _ = std::fs::write(Self::keybindings_path(), kb);
        }
    }

    pub fn note_folder(&mut self, folder: &str) {
        self.last_folder = Some(folder.to_string());
        self.recent_folders.retain(|f| f != folder);
        self.recent_folders.insert(0, folder.to_string());
        self.recent_folders.truncate(12);
        self.save();
    }

    pub fn line_px(&self) -> f32 {
        (self.font_size * self.line_height).round()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabSession {
    pub path: String,
    pub caret: usize,
    pub scroll: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub folder: Option<String>,
    pub tabs: Vec<TabSession>,
    pub active: usize,
}

impl Session {
    pub fn load() -> Self {
        std::fs::read_to_string(Settings::session_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = Settings::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Settings::session_path(), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn autosave_is_enabled_by_default() {
        assert!(Settings::default().autosave);
    }
}
