//! Persisted user settings, custom keybindings, and the last editor session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

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
    pub format_on_save: bool,
    pub trim_trailing_whitespace: bool,
    pub auto_close_pairs: bool,
    pub detect_indentation: bool,
    /// Recent project-search queries, newest first (capped at 10).
    pub search_history: Vec<String>,
    /// Recently run palette command ids, newest first (capped at 6).
    pub recent_commands: Vec<String>,
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
            format_on_save: false,
            trim_trailing_whitespace: false,
            auto_close_pairs: true,
            detect_indentation: true,
            search_history: Vec::new(),
            recent_commands: Vec::new(),
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

    pub fn recovery_dir() -> PathBuf {
        Self::config_dir().join("recovery")
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

    /// Mark the settings dirty. The hot paths (ctrl+wheel zoom, view toggles)
    /// used to rewrite settings.json per event; the app now flushes at most
    /// once per quiet second and force-flushes on exit.
    pub fn save(&self) {
        if let Ok(mut state) = SAVE_STATE.lock() {
            state.dirty = true;
            state.requested = Instant::now();
        }
    }

    /// Write immediately and drop any pending debounce.
    pub fn flush(&self) {
        if let Ok(mut state) = SAVE_STATE.lock() {
            state.dirty = false;
        }
        write_settings(self);
    }

    /// True once the debounce window has elapsed with no new change.
    pub fn save_due() -> bool {
        SAVE_STATE
            .lock()
            .is_ok_and(|state| state.dirty && state.requested.elapsed() >= SAVE_IDLE)
    }

    /// Time left before the debounced flush, for the frame wake timer.
    pub fn save_delay() -> Option<Duration> {
        SAVE_STATE.lock().ok().and_then(|state| {
            state.dirty.then(|| {
                SAVE_IDLE
                    .saturating_sub(state.requested.elapsed())
                    .max(Duration::from_millis(1))
            })
        })
    }

    pub fn note_folder(&mut self, folder: &str) {
        self.last_folder = Some(folder.to_string());
        self.recent_folders.retain(|f| f != folder);
        self.recent_folders.insert(0, folder.to_string());
        self.recent_folders.truncate(12);
        self.save();
    }

    /// Remember a project-search query; newest first, capped at 10.
    pub fn push_search_history(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.search_history.retain(|entry| entry != query);
        self.search_history.insert(0, query.to_string());
        self.search_history.truncate(10);
        self.save();
    }

    /// Remember a palette command; newest first, capped at 6.
    pub fn push_recent_command(&mut self, id: &str) {
        if self.recent_commands.first().is_some_and(|entry| entry == id) {
            return;
        }
        self.recent_commands.retain(|entry| entry != id);
        self.recent_commands.insert(0, id.to_string());
        self.recent_commands.truncate(6);
        self.save();
    }

    pub fn line_px(&self) -> f32 {
        (self.font_size * self.line_height).round()
    }
}

/// Quiet period before a dirty settings file reaches the disk.
const SAVE_IDLE: Duration = Duration::from_millis(1000);

struct SaveState {
    dirty: bool,
    requested: Instant,
}

static SAVE_STATE: LazyLock<Mutex<SaveState>> = LazyLock::new(|| {
    Mutex::new(SaveState {
        dirty: false,
        requested: Instant::now(),
    })
});

fn write_settings(settings: &Settings) {
    let dir = Settings::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(Settings::settings_path(), text);
    }
    // Always write the file: clearing every override must remove stale ones too.
    if let Ok(kb) = serde_json::to_string_pretty(&settings.keybindings) {
        let _ = std::fs::write(Settings::keybindings_path(), kb);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabSession {
    pub path: String,
    pub caret: usize,
    pub scroll: f32,
}

/// One editor group (split pane) of the restored window.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupSession {
    pub tabs: Vec<TabSession>,
    pub active: usize,
}

/// Saved shell instance: kind plus working directory, restored on startup.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSessionState {
    pub shell: String,
    pub cwd: String,
}

impl Default for TerminalSessionState {
    fn default() -> Self {
        Self {
            shell: "default".to_string(),
            cwd: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Flat tab list, kept for older builds and as the fallback when groups is empty.
    pub folder: Option<String>,
    pub tabs: Vec<TabSession>,
    pub active: usize,
    pub untitled: Vec<UntitledSession>,
    /// Split layout; each group is an ordered tab list plus its active index.
    pub groups: Vec<GroupSession>,
    pub active_group: usize,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub sidebar_mode: String,
    pub bottom_visible: bool,
    pub bottom_tab: String,
    pub bottom_height: f32,
    pub terminals: Vec<TerminalSessionState>,
    pub active_terminal: usize,
    /// Last project-search query, restored into the search sidebar.
    pub search_query: String,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            folder: None,
            tabs: Vec::new(),
            active: 0,
            untitled: Vec::new(),
            groups: Vec::new(),
            active_group: 0,
            sidebar_visible: true,
            sidebar_width: 268.0,
            sidebar_mode: "explorer".to_string(),
            bottom_visible: false,
            bottom_tab: "terminal".to_string(),
            bottom_height: 240.0,
            terminals: Vec::new(),
            active_terminal: 0,
            search_query: String::new(),
        }
    }
}

/// One unsaved buffer carried across restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UntitledSession {
    /// Inline text; `None` when the text lives in the recovery file instead.
    pub text: Option<String>,
    /// Recovery file for buffers too large to inline (>512 KB).
    pub path: Option<String>,
    pub language: String,
    pub caret: usize,
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
    use super::{GroupSession, Session, Settings, TabSession, TerminalSessionState, UntitledSession};

    #[test]
    fn autosave_is_enabled_by_default() {
        assert!(Settings::default().autosave);
    }

    #[test]
    fn new_preference_defaults_and_legacy_settings_parse() {
        let defaults = Settings::default();
        assert!(!defaults.format_on_save);
        assert!(!defaults.trim_trailing_whitespace);
        assert!(defaults.auto_close_pairs);
        assert!(defaults.detect_indentation);
        assert!(defaults.scroll_past_end);
        assert!(defaults.search_history.is_empty());

        let legacy = r#"{"theme":"light","font_size":15.0}"#;
        let parsed: Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.theme, "light");
        assert!(parsed.auto_close_pairs);
        assert!(parsed.search_history.is_empty());
    }

    #[test]
    fn search_history_and_recent_commands_cap_and_dedupe() {
        let mut settings = Settings::default();
        for index in 0..14 {
            settings.push_search_history(&format!("q{index}"));
        }
        settings.push_search_history("q13");
        assert_eq!(settings.search_history.len(), 10);
        assert_eq!(settings.search_history[0], "q13");
        assert_eq!(settings.search_history.iter().filter(|q| *q == "q13").count(), 1);

        for index in 0..9 {
            settings.push_recent_command(&format!("cmd.{index}"));
        }
        assert_eq!(settings.recent_commands.len(), 6);
        assert_eq!(settings.recent_commands[0], "cmd.8");
    }

    #[test]
    fn legacy_session_without_untitled_still_parses() {
        let json = r#"{"folder":null,"tabs":[{"path":"a.rs","caret":3,"scroll":0.0}],"active":0}"#;
        let session: Session = serde_json::from_str(json).unwrap();
        assert_eq!(session.tabs.len(), 1);
        assert!(session.untitled.is_empty());
        // Fields introduced by the B5b session expansion fall back to usable defaults.
        assert!(session.groups.is_empty());
        assert!(session.sidebar_visible);
        assert_eq!(session.sidebar_width, 268.0);
        assert_eq!(session.sidebar_mode, "explorer");
        assert!(!session.bottom_visible);
        assert_eq!(session.bottom_tab, "terminal");
        assert!(session.terminals.is_empty());
        assert_eq!(session.search_query, "");
    }

    #[test]
    fn session_roundtrips_split_panels_terminals_and_query() {
        let session = Session {
            groups: vec![
                GroupSession {
                    tabs: vec![TabSession {
                        path: "a.rs".to_string(),
                        caret: 7,
                        scroll: 12.0,
                    }],
                    active: 0,
                },
                GroupSession::default(),
            ],
            active_group: 1,
            sidebar_visible: false,
            sidebar_width: 300.0,
            sidebar_mode: "git".to_string(),
            bottom_visible: true,
            bottom_tab: "output".to_string(),
            bottom_height: 321.0,
            terminals: vec![TerminalSessionState {
                shell: "cmd".to_string(),
                cwd: "D:/work".to_string(),
            }],
            active_terminal: 0,
            search_query: "fn main".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.groups.len(), 2);
        assert_eq!(back.groups[0].tabs[0].caret, 7);
        assert_eq!(back.active_group, 1);
        assert!(!back.sidebar_visible);
        assert_eq!(back.sidebar_mode, "git");
        assert!(back.bottom_visible);
        assert_eq!(back.bottom_tab, "output");
        assert_eq!(back.bottom_height, 321.0);
        assert_eq!(back.terminals[0].shell, "cmd");
        assert_eq!(back.terminals[0].cwd, "D:/work");
        assert_eq!(back.search_query, "fn main");
    }

    #[test]
    fn untitled_entry_roundtrips() {
        let session = Session {
            untitled: vec![UntitledSession {
                text: Some("hi".to_string()),
                path: None,
                language: "Rust".to_string(),
                caret: 2,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.untitled.len(), 1);
        assert_eq!(back.untitled[0].text.as_deref(), Some("hi"));
        assert_eq!(back.untitled[0].caret, 2);
    }
}
