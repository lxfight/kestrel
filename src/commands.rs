//! The command registry: every user-facing action, its label, icon, and default shortcut.

use egui::{Key, Modifiers};

use crate::icons::Icon;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Command {
    // File
    NewFile,
    NewFolder,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseTab,
    CloseAllTabs,
    ReloadFile,
    ToggleEol,
    ReopenUtf8,
    ReopenUtf8Bom,
    ReopenUtf16Le,
    ReopenUtf16Be,
    SaveAsUtf8,
    SaveAsUtf8Bom,
    SaveAsUtf16Le,
    SaveAsUtf16Be,
    // Edit & Multi-cursor
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    SelectNextOccurrence,
    SelectAllOccurrences,
    AddCursorUp,
    AddCursorDown,
    Find,
    Replace,
    FindNext,
    FindPrev,
    GotoLine,
    GotoSymbol,
    GotoDefinition,
    FormatDocument,
    TriggerSuggest,
    ToggleComment,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    Indent,
    Outdent,
    ToggleFold,
    FoldAll,
    UnfoldAll,
    SelectExpand,
    SelectShrink,
    JumpToBracket,
    TrimTrailingWhitespace,
    ToggleAutoClose,
    JumpBack,
    JumpForward,
    // Navigation / view / workbench
    ShowPalette,
    QuickOpen,
    ToggleSidebar,
    ToggleTerminal,
    FocusTerminal,
    NewTerminal,
    TerminalClose,
    TerminalClear,
    TerminalSplit,
    TerminalUnsplit,
    TerminalNext,
    TerminalPrev,
    TerminalMaximize,
    ShowProblems,
    ShowOutput,
    SplitEditor,
    ToggleMarkdownPreview,
    MarkdownPreviewSide,
    PreviewInBrowser,
    ToggleWordWrap,
    ToggleMinimap,
    ToggleStickyScroll,
    ToggleWhitespace,
    ToggleIndentGuides,
    ToggleIndent,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ThemeDark,
    ThemeLight,
    OpenSettings,
    SetLanguage,
    ShowExplorer,
    ShowSearch,
    ShowGit,
    ShowOutline,
    NextTab,
    PrevTab,
    // Workspace
    CopyPath,
    RevealInExplorer,
    RefreshExplorer,
    // Help
    CheckForUpdates,
    About,
    Shortcuts,
    // Extensions
    Plugins,
}

/// A key chord with exact modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub mods: Modifiers,
    pub key: Key,
}

impl Shortcut {
    pub const fn new(mods: Modifiers, key: Key) -> Self {
        Self { mods, key }
    }

    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.mods.ctrl || self.mods.command {
            if cfg!(target_os = "macos") {
                s.push_str("Cmd+");
            } else {
                s.push_str("Ctrl+");
            }
        }
        if self.mods.shift {
            s.push_str("Shift+");
        }
        if self.mods.alt {
            s.push_str("Alt+");
        }
        s.push_str(key_name(self.key));
        s
    }

    /// Parse a keybinding such as `ctrl+alt+h`, `f12`, or `ctrl+shift+space`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut mods = Modifiers::NONE;
        let mut key = None;
        let mut parts: Vec<&str> = text.split('+').collect();
        let mut plus_key = false;
        if parts.len() > 1 && parts.last().is_some_and(|part| part.trim().is_empty()) {
            parts.pop();
            if parts.last().is_some_and(|part| part.trim().is_empty()) {
                parts.pop();
                plus_key = true;
            }
        }
        if plus_key {
            key = Some(Key::Plus);
        }
        for part in parts {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                // All primary-modifier aliases map to egui's platform `command` bit:
                // Ctrl on Windows/Linux, ⌘ on macOS.
                "ctrl" | "control" | "cmd" | "command" | "super" | "win" | "meta" => {
                    mods.command = true;
                }
                "shift" => mods.shift = true,
                "alt" | "option" | "opt" => mods.alt = true,
                _ => {
                    if key.is_some() {
                        return None;
                    }
                    key = parse_key(part);
                }
            }
        }
        if plus_key && key != Some(Key::Plus) {
            return None;
        }
        key.map(|key| Self { mods, key })
    }

    /// Parse several keybindings separated by commas; invalid entries are skipped.
    pub fn parse_list(text: &str) -> Vec<Self> {
        text.split(',').filter_map(Self::parse).collect()
    }
}

/// Primary platform modifier: Ctrl on Windows/Linux, ⌘ on macOS.
const CTRL: Modifiers = Modifiers::COMMAND;

const fn ctrl_shift() -> Modifiers {
    Modifiers::COMMAND.plus(Modifiers::SHIFT)
}

const fn ctrl_alt() -> Modifiers {
    Modifiers::COMMAND.plus(Modifiers::ALT)
}

const fn shift_alt() -> Modifiers {
    Modifiers {
        alt: true,
        ctrl: false,
        shift: true,
        mac_cmd: false,
        command: false,
    }
}

const fn alt() -> Modifiers {
    Modifiers {
        alt: true,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: false,
    }
}

const fn no_mods() -> Modifiers {
    Modifiers {
        alt: false,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: false,
    }
}


fn parse_key(part: &str) -> Option<Key> {
    let lower = part.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "up" | "↑" => Some(Key::ArrowUp),
        "down" | "↓" => Some(Key::ArrowDown),
        "left" | "←" => Some(Key::ArrowLeft),
        "right" | "→" => Some(Key::ArrowRight),
        "esc" | "escape" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "enter" | "return" => Some(Key::Enter),
        "delete" | "del" => Some(Key::Delete),
        "insert" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "space" => Some(Key::Space),
        _ => None,
    };
    alias.or_else(|| Key::from_name(part)).or_else(|| {
        let mut chars = part.chars();
        let first = chars.next()?;
        let capitalized = first.to_uppercase().collect::<String>() + chars.as_str();
        Key::from_name(&capitalized)
    })
}

fn key_name(key: Key) -> &'static str {
    match key {
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F5 => "F5",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::Escape => "Esc",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Delete => "Delete",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Plus => "Plus",
        Key::Minus => "-",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Backslash => "\\",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Backtick => "`",
        Key::Equals => "=",
        Key::Space => "Space",
        Key::Num0 => "0",
        Key::A => "A",
        Key::B => "B",
        Key::C => "C",
        Key::D => "D",
        Key::E => "E",
        Key::F => "F",
        Key::G => "G",
        Key::H => "H",
        Key::I => "I",
        Key::J => "J",
        Key::K => "K",
        Key::L => "L",
        Key::M => "M",
        Key::N => "N",
        Key::O => "O",
        Key::P => "P",
        Key::Q => "Q",
        Key::R => "R",
        Key::S => "S",
        Key::T => "T",
        Key::U => "U",
        Key::V => "V",
        Key::W => "W",
        Key::X => "X",
        Key::Y => "Y",
        Key::Z => "Z",
        _ => "",
    }
}

impl Command {

    /// Stable identifier used by keybindings, plugins, and settings files.
    pub fn id(self) -> &'static str {
        match self {
            Command::NewFile => "file.new",
            Command::NewFolder => "file.newFolder",
            Command::OpenFile => "file.openFile",
            Command::OpenFolder => "file.openFolder",
            Command::Save => "file.save",
            Command::SaveAs => "file.saveAs",
            Command::SaveAll => "file.saveAll",
            Command::CloseTab => "file.closeTab",
            Command::CloseAllTabs => "file.closeAllTabs",
            Command::ReloadFile => "file.reload",
            Command::ToggleEol => "file.toggleEol",
            Command::ReopenUtf8 => "file.reopenUtf8",
            Command::ReopenUtf8Bom => "file.reopenUtf8Bom",
            Command::ReopenUtf16Le => "file.reopenUtf16le",
            Command::ReopenUtf16Be => "file.reopenUtf16be",
            Command::SaveAsUtf8 => "file.saveAsUtf8",
            Command::SaveAsUtf8Bom => "file.saveAsUtf8Bom",
            Command::SaveAsUtf16Le => "file.saveAsUtf16le",
            Command::SaveAsUtf16Be => "file.saveAsUtf16be",
            Command::Undo => "edit.undo",
            Command::Redo => "edit.redo",
            Command::Cut => "edit.cut",
            Command::Copy => "edit.copy",
            Command::Paste => "edit.paste",
            Command::SelectAll => "edit.selectAll",
            Command::SelectNextOccurrence => "edit.selectNextOccurrence",
            Command::SelectAllOccurrences => "edit.selectAllOccurrences",
            Command::AddCursorUp => "edit.addCursorUp",
            Command::AddCursorDown => "edit.addCursorDown",
            Command::Find => "edit.find",
            Command::Replace => "edit.replace",
            Command::FindNext => "edit.findNext",
            Command::FindPrev => "edit.findPrev",
            Command::GotoLine => "edit.gotoLine",
            Command::GotoSymbol => "edit.gotoSymbol",
            Command::GotoDefinition => "edit.gotoDefinition",
            Command::FormatDocument => "edit.formatDocument",
            Command::TriggerSuggest => "edit.triggerSuggest",
            Command::ToggleComment => "edit.toggleComment",
            Command::DuplicateLine => "edit.duplicateLine",
            Command::DeleteLine => "edit.deleteLine",
            Command::MoveLineUp => "edit.moveLineUp",
            Command::MoveLineDown => "edit.moveLineDown",
            Command::Indent => "edit.indent",
            Command::Outdent => "edit.outdent",
            Command::ToggleFold => "edit.toggleFold",
            Command::FoldAll => "edit.foldAll",
            Command::UnfoldAll => "edit.unfoldAll",
            Command::SelectExpand => "edit.selectExpand",
            Command::SelectShrink => "edit.selectShrink",
            Command::JumpToBracket => "edit.jumpToBracket",
            Command::TrimTrailingWhitespace => "edit.trimTrailingWhitespace",
            Command::ToggleAutoClose => "edit.toggleAutoClose",
            Command::JumpBack => "edit.jumpBack",
            Command::JumpForward => "edit.jumpForward",
            Command::ShowPalette => "view.commandPalette",
            Command::QuickOpen => "view.quickOpen",
            Command::ToggleSidebar => "view.toggleSidebar",
            Command::ToggleTerminal => "view.toggleTerminal",
            Command::FocusTerminal => "terminal.focus",
            Command::NewTerminal => "view.newTerminal",
            Command::TerminalClose => "terminal.close",
            Command::TerminalClear => "terminal.clear",
            Command::TerminalSplit => "terminal.split",
            Command::TerminalUnsplit => "terminal.unsplit",
            Command::TerminalNext => "terminal.next",
            Command::TerminalPrev => "terminal.prev",
            Command::TerminalMaximize => "terminal.toggleMaximize",
            Command::ShowProblems => "view.problems",
            Command::ShowOutput => "view.output",
            Command::SplitEditor => "view.splitEditor",
            Command::ToggleMarkdownPreview => "view.toggleMarkdownPreview",
            Command::MarkdownPreviewSide => "view.markdownPreviewSide",
            Command::PreviewInBrowser => "view.previewInBrowser",
            Command::ToggleWordWrap => "view.toggleWordWrap",
            Command::ToggleMinimap => "view.toggleMinimap",
            Command::ToggleStickyScroll => "view.toggleStickyScroll",
            Command::ToggleWhitespace => "view.toggleWhitespace",
            Command::ToggleIndentGuides => "view.toggleIndentGuides",
            Command::ToggleIndent => "edit.toggleIndent",
            Command::ZoomIn => "view.zoomIn",
            Command::ZoomOut => "view.zoomOut",
            Command::ZoomReset => "view.zoomReset",
            Command::ThemeDark => "view.themeDark",
            Command::ThemeLight => "view.themeLight",
            Command::OpenSettings => "view.settings",
            Command::SetLanguage => "view.setLanguage",
            Command::ShowExplorer => "view.explorer",
            Command::ShowSearch => "view.search",
            Command::ShowGit => "view.git",
            Command::ShowOutline => "view.outline",
            Command::NextTab => "view.nextTab",
            Command::PrevTab => "view.prevTab",
            Command::CopyPath => "workspace.copyPath",
            Command::RevealInExplorer => "workspace.revealInExplorer",
            Command::RefreshExplorer => "workspace.refreshExplorer",
            Command::CheckForUpdates => "help.checkForUpdates",
            Command::About => "help.about",
            Command::Shortcuts => "help.shortcuts",
            Command::Plugins => "plugins.manage",
        }
    }

    /// Look up a built-in command by its stable identifier.
    pub fn from_id(id: &str) -> Option<Command> {
        Command::ALL.iter().copied().find(|command| command.id() == id)
    }

    pub const ALL: &'static [Command] = &[
        Command::NewFile,
        Command::NewFolder,
        Command::OpenFile,
        Command::OpenFolder,
        Command::Save,
        Command::SaveAs,
        Command::SaveAll,
        Command::CloseTab,
        Command::CloseAllTabs,
        Command::ReloadFile,
        Command::ToggleEol,
        Command::ReopenUtf8,
        Command::ReopenUtf8Bom,
        Command::ReopenUtf16Le,
        Command::ReopenUtf16Be,
        Command::SaveAsUtf8,
        Command::SaveAsUtf8Bom,
        Command::SaveAsUtf16Le,
        Command::SaveAsUtf16Be,
        Command::Undo,
        Command::Redo,
        Command::Cut,
        Command::Copy,
        Command::Paste,
        Command::SelectAll,
        Command::SelectNextOccurrence,
        Command::SelectAllOccurrences,
        Command::AddCursorUp,
        Command::AddCursorDown,
        Command::Find,
        Command::Replace,
        Command::FindNext,
        Command::FindPrev,
        Command::GotoLine,
        Command::GotoSymbol,
        Command::GotoDefinition,
        Command::FormatDocument,
        Command::TriggerSuggest,
        Command::ToggleComment,
        Command::DuplicateLine,
        Command::DeleteLine,
        Command::MoveLineUp,
        Command::MoveLineDown,
        Command::Indent,
        Command::Outdent,
        Command::ToggleFold,
        Command::FoldAll,
        Command::UnfoldAll,
        Command::SelectExpand,
        Command::SelectShrink,
        Command::JumpToBracket,
        Command::TrimTrailingWhitespace,
        Command::ToggleAutoClose,
        Command::JumpBack,
        Command::JumpForward,
        Command::ShowPalette,
        Command::QuickOpen,
        Command::ToggleSidebar,
        Command::ToggleTerminal,
        Command::FocusTerminal,
        Command::NewTerminal,
        Command::TerminalClose,
        Command::TerminalClear,
        Command::TerminalSplit,
        Command::TerminalUnsplit,
        Command::TerminalNext,
        Command::TerminalPrev,
        Command::TerminalMaximize,
        Command::ShowProblems,
        Command::ShowOutput,
        Command::SplitEditor,
        Command::ToggleMarkdownPreview,
        Command::MarkdownPreviewSide,
        Command::PreviewInBrowser,
        Command::ToggleWordWrap,
        Command::ToggleMinimap,
        Command::ToggleStickyScroll,
        Command::ToggleWhitespace,
        Command::ToggleIndentGuides,
        Command::ToggleIndent,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::ZoomReset,
        Command::ThemeDark,
        Command::ThemeLight,
        Command::OpenSettings,
        Command::SetLanguage,
        Command::ShowExplorer,
        Command::ShowSearch,
        Command::ShowGit,
        Command::ShowOutline,
        Command::NextTab,
        Command::PrevTab,
        Command::CopyPath,
        Command::RevealInExplorer,
        Command::RefreshExplorer,
        Command::CheckForUpdates,
        Command::About,
        Command::Shortcuts,
        Command::Plugins,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::NewFile => "新建文件",
            Command::NewFolder => "新建文件夹",
            Command::OpenFile => "打开文件…",
            Command::OpenFolder => "打开文件夹…",
            Command::Save => "保存",
            Command::SaveAs => "另存为…",
            Command::SaveAll => "全部保存",
            Command::CloseTab => "关闭编辑器",
            Command::CloseAllTabs => "关闭所有编辑器",
            Command::ReloadFile => "从磁盘重新加载",
            Command::ToggleEol => "切换行尾 (LF/CRLF)",
            Command::ReopenUtf8 => "以 UTF-8 重新打开",
            Command::ReopenUtf8Bom => "以 UTF-8 BOM 重新打开",
            Command::ReopenUtf16Le => "以 UTF-16 LE 重新打开",
            Command::ReopenUtf16Be => "以 UTF-16 BE 重新打开",
            Command::SaveAsUtf8 => "另存为 UTF-8…",
            Command::SaveAsUtf8Bom => "另存为 UTF-8 BOM…",
            Command::SaveAsUtf16Le => "另存为 UTF-16 LE…",
            Command::SaveAsUtf16Be => "另存为 UTF-16 BE…",
            Command::Undo => "撤销",
            Command::Redo => "重做",
            Command::Cut => "剪切",
            Command::Copy => "复制",
            Command::Paste => "粘贴",
            Command::SelectAll => "全选",
            Command::SelectNextOccurrence => "添加下一个匹配项到选区",
            Command::SelectAllOccurrences => "选择所有匹配项 (多光标)",
            Command::AddCursorUp => "在上方添加光标",
            Command::AddCursorDown => "在下方添加光标",
            Command::Find => "查找",
            Command::Replace => "替换",
            Command::FindNext => "查找下一个",
            Command::FindPrev => "查找上一个",
            Command::GotoLine => "转到行…",
            Command::GotoSymbol => "转到文件中的符号…",
            Command::GotoDefinition => "转到定义",
            Command::FormatDocument => "格式化文档",
            Command::TriggerSuggest => "触发代码补全建议",
            Command::ToggleComment => "切换行注释",
            Command::DuplicateLine => "复制行",
            Command::DeleteLine => "删除行",
            Command::MoveLineUp => "上移行",
            Command::MoveLineDown => "下移行",
            Command::Indent => "增加缩进",
            Command::Outdent => "减少缩进",
            Command::ToggleFold => "切换代码折叠",
            Command::FoldAll => "全部折叠代码块",
            Command::UnfoldAll => "全部展开代码块",
            Command::SelectExpand => "扩大选区",
            Command::SelectShrink => "缩小选区",
            Command::JumpToBracket => "跳转到匹配括号",
            Command::TrimTrailingWhitespace => "清理行尾空白",
            Command::ToggleAutoClose => "切换自动闭合括号",
            Command::JumpBack => "后退（跳转历史）",
            Command::JumpForward => "前进（跳转历史）",
            Command::ShowPalette => "显示所有命令",
            Command::QuickOpen => "快速打开文件",
            Command::ToggleSidebar => "切换侧边栏",
            Command::ToggleTerminal => "切换终端面板",
            Command::FocusTerminal => "聚焦终端（或打开终端）",
            Command::NewTerminal => "新建终端实例",
            Command::TerminalClose => "关闭当前终端会话",
            Command::TerminalClear => "清空当前终端输出",
            Command::TerminalSplit => "向右拆分终端窗格",
            Command::TerminalUnsplit => "取消终端分屏（仅保留当前窗格）",
            Command::TerminalNext => "切换到下一个终端",
            Command::TerminalPrev => "切换到上一个终端",
            Command::TerminalMaximize => "最大化 / 还原底部终端面板",
            Command::ShowProblems => "显示问题面板 (Problems)",
            Command::ShowOutput => "显示输出面板 (Output)",
            Command::SplitEditor => "向右拆分编辑器 (分屏)",
            Command::ToggleMarkdownPreview => "切换 Markdown / 网页预览",
            Command::MarkdownPreviewSide => "Markdown / 网页侧边分屏预览",
            Command::PreviewInBrowser => "在浏览器中预览网页",
            Command::ToggleWordWrap => "切换自动换行 (Word Wrap)",
            Command::ToggleMinimap => "切换小地图 (Minimap)",
            Command::ToggleStickyScroll => "切换粘性滚动 (Sticky Scroll)",
            Command::ToggleWhitespace => "切换空白字符显示",
            Command::ToggleIndentGuides => "切换缩进参考线",
            Command::ToggleIndent => "切换缩进（空格 / 制表符）",
            Command::ZoomIn => "放大界面",
            Command::ZoomOut => "缩小界面",
            Command::ZoomReset => "重置界面缩放",
            Command::ThemeDark => "使用深色主题",
            Command::ThemeLight => "使用浅色主题",
            Command::OpenSettings => "打开设置",
            Command::SetLanguage => "更改语言模式…",
            Command::ShowExplorer => "显示资源管理器",
            Command::ShowSearch => "显示全局搜索",
            Command::ShowGit => "显示源代码管理 (Git)",
            Command::ShowOutline => "显示代码大纲 (Outline)",
            Command::NextTab => "下一个编辑器",
            Command::PrevTab => "上一个编辑器",
            Command::CopyPath => "复制文件路径",
            Command::RevealInExplorer => "在文件资源管理器中显示",
            Command::RefreshExplorer => "刷新资源管理器",
            Command::CheckForUpdates => "检查更新…",
            Command::About => "关于 Kestrel",
            Command::Shortcuts => "键盘快捷键",
            Command::Plugins => "管理插件",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Command::NewFile => Icon::NewFile,
            Command::NewFolder => Icon::NewFolder,
            Command::OpenFile | Command::QuickOpen => Icon::File,
            Command::OpenFolder => Icon::FolderOpen,
            Command::Save | Command::SaveAs => Icon::Save,
            Command::SaveAll => Icon::SaveAll,
            Command::CloseTab | Command::CloseAllTabs => Icon::Close,
            Command::ReloadFile | Command::RefreshExplorer => Icon::Refresh,
            Command::ReopenUtf8
            | Command::ReopenUtf8Bom
            | Command::ReopenUtf16Le
            | Command::ReopenUtf16Be => Icon::Refresh,
            Command::JumpBack | Command::JumpForward | Command::JumpToBracket => Icon::Goforward,
            Command::Undo => Icon::Undo,
            Command::Redo => Icon::Redo,
            Command::Copy | Command::CopyPath => Icon::Copy,
            Command::Find | Command::FindNext | Command::FindPrev | Command::ShowSearch => {
                Icon::Search
            }
            Command::Replace => Icon::Replace,
            Command::GotoLine | Command::GotoDefinition => Icon::Goforward,
            Command::GotoSymbol | Command::ShowOutline => Icon::Outline,
            Command::FormatDocument => Icon::FileCode,
            Command::TriggerSuggest => Icon::Lightbulb,
            Command::ToggleFold | Command::FoldAll => Icon::CollapseAll,
            Command::UnfoldAll => Icon::ExpandAll,
            Command::ToggleSidebar | Command::ShowExplorer => Icon::Explorer,
            Command::ToggleTerminal
            | Command::FocusTerminal
            | Command::NewTerminal
            | Command::TerminalClose
            | Command::TerminalNext
            | Command::TerminalPrev
            | Command::TerminalMaximize => Icon::Terminal,
            Command::TerminalClear => Icon::ClearAll,
            Command::TerminalSplit | Command::TerminalUnsplit => Icon::SplitHorizontal,
            Command::ShowProblems => Icon::Problems,
            Command::ShowOutput => Icon::Output,
            Command::SplitEditor | Command::MarkdownPreviewSide => Icon::SplitHorizontal,
            Command::ToggleMarkdownPreview => Icon::Preview,
            Command::PreviewInBrowser => Icon::Globe,
            Command::ToggleWordWrap => Icon::WordWrap,
            Command::ShowGit => Icon::SourceControl,
            Command::OpenSettings => Icon::Settings,
            Command::ThemeLight => Icon::Sun,
            Command::ThemeDark => Icon::Moon,
            Command::CheckForUpdates => Icon::CloudDownload,
            Command::Shortcuts => Icon::Keyboard,
            Command::About => Icon::Info,
            Command::Plugins => Icon::Extensions,
            _ => Icon::FileCode,
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Command::NewFile
            | Command::NewFolder
            | Command::OpenFile
            | Command::OpenFolder
            | Command::Save
            | Command::SaveAs
            | Command::SaveAll
            | Command::CloseTab
            | Command::CloseAllTabs
            | Command::ReloadFile
            | Command::ToggleEol
            | Command::ReopenUtf8
            | Command::ReopenUtf8Bom
            | Command::ReopenUtf16Le
            | Command::ReopenUtf16Be
            | Command::SaveAsUtf8
            | Command::SaveAsUtf8Bom
            | Command::SaveAsUtf16Le
            | Command::SaveAsUtf16Be => "文件",
            Command::Undo
            | Command::Redo
            | Command::Cut
            | Command::Copy
            | Command::Paste
            | Command::SelectAll
            | Command::SelectNextOccurrence
            | Command::SelectAllOccurrences
            | Command::AddCursorUp
            | Command::AddCursorDown
            | Command::Find
            | Command::Replace
            | Command::FindNext
            | Command::FindPrev
            | Command::GotoLine
            | Command::GotoSymbol
            | Command::GotoDefinition
            | Command::FormatDocument
            | Command::TriggerSuggest
            | Command::ToggleComment
            | Command::DuplicateLine
            | Command::DeleteLine
            | Command::MoveLineUp
            | Command::MoveLineDown
            | Command::Indent
            | Command::Outdent
            | Command::ToggleFold
            | Command::FoldAll
            | Command::UnfoldAll
            | Command::SelectExpand
            | Command::SelectShrink
            | Command::JumpToBracket
            | Command::TrimTrailingWhitespace
            | Command::ToggleAutoClose
            | Command::JumpBack
            | Command::JumpForward
            | Command::ToggleIndent => "编辑",
            Command::CopyPath | Command::RevealInExplorer | Command::RefreshExplorer => "工作区",
            Command::CheckForUpdates | Command::About | Command::Shortcuts => "帮助",
            Command::Plugins => "插件",
            _ => "视图",
        }
    }

    pub fn shortcut(self) -> Option<Shortcut> {
        use Key::*;
        Some(match self {
            Command::NewFile => Shortcut::new(CTRL, N),
            Command::OpenFile => Shortcut::new(CTRL, O),
            Command::OpenFolder => Shortcut::new(ctrl_shift(), O),
            Command::Save => Shortcut::new(CTRL, S),
            Command::SaveAs => Shortcut::new(ctrl_shift(), S),
            Command::SaveAll => Shortcut::new(ctrl_shift(), A),
            Command::CloseTab => Shortcut::new(CTRL, W),
            Command::CloseAllTabs => Shortcut::new(ctrl_shift(), W),
            Command::Undo => Shortcut::new(CTRL, Z),
            Command::Redo => Shortcut::new(CTRL, Y),
            Command::Cut => Shortcut::new(CTRL, X),
            Command::Copy => Shortcut::new(CTRL, C),
            Command::Paste => Shortcut::new(CTRL, V),
            Command::SelectAll => Shortcut::new(CTRL, A),
            Command::SelectNextOccurrence => Shortcut::new(CTRL, D),
            Command::SelectAllOccurrences => Shortcut::new(ctrl_shift(), L),
            Command::AddCursorUp => Shortcut::new(ctrl_alt(), ArrowUp),
            Command::AddCursorDown => Shortcut::new(ctrl_alt(), ArrowDown),
            Command::Find => Shortcut::new(CTRL, F),
            Command::Replace => Shortcut::new(CTRL, H),
            Command::FindNext => Shortcut::new(no_mods(), F3),
            Command::FindPrev => Shortcut::new(ctrl_shift(), F3),
            Command::GotoLine => Shortcut::new(CTRL, G),
            Command::GotoSymbol => Shortcut::new(ctrl_shift(), Period),
            Command::GotoDefinition => Shortcut::new(no_mods(), F12),
            Command::FormatDocument => Shortcut::new(shift_alt(), F),
            Command::TriggerSuggest => Shortcut::new(CTRL, Space),
            Command::ToggleComment => Shortcut::new(CTRL, Slash),
            Command::DuplicateLine => Shortcut::new(ctrl_shift(), D),
            Command::DeleteLine => Shortcut::new(ctrl_shift(), K),
            Command::MoveLineUp => Shortcut::new(alt(), ArrowUp),
            Command::MoveLineDown => Shortcut::new(alt(), ArrowDown),
            Command::ToggleFold => Shortcut::new(ctrl_shift(), OpenBracket),
            Command::JumpToBracket => Shortcut::new(ctrl_shift(), Backslash),
            Command::SelectExpand => Shortcut::new(shift_alt(), ArrowRight),
            Command::SelectShrink => Shortcut::new(shift_alt(), ArrowLeft),
            Command::JumpBack => Shortcut::new(alt(), ArrowLeft),
            Command::JumpForward => Shortcut::new(alt(), ArrowRight),
            Command::ShowPalette => Shortcut::new(ctrl_shift(), P),
            Command::QuickOpen => Shortcut::new(CTRL, P),
            Command::ToggleSidebar => Shortcut::new(CTRL, B),
            Command::ToggleTerminal => Shortcut::new(CTRL, J),
            Command::FocusTerminal => Shortcut::new(CTRL, Backtick),
            Command::NewTerminal => Shortcut::new(ctrl_shift(), Backtick),
            Command::ShowProblems => Shortcut::new(ctrl_shift(), M),
            Command::ShowOutput => Shortcut::new(ctrl_shift(), U),
            Command::SplitEditor => Shortcut::new(CTRL, Backslash),
            Command::ToggleMarkdownPreview => Shortcut::new(ctrl_shift(), V),
            Command::MarkdownPreviewSide => Shortcut::new(alt(), V),
            Command::ToggleWordWrap => Shortcut::new(alt(), Z),
            Command::ToggleMinimap => Shortcut::new(alt(), M),
            Command::ToggleIndentGuides => Shortcut::new(ctrl_shift(), G),
            Command::ZoomIn => Shortcut::new(CTRL, Plus),
            Command::ZoomOut => Shortcut::new(CTRL, Minus),
            Command::ZoomReset => Shortcut::new(CTRL, Num0),
            Command::ShowExplorer => Shortcut::new(ctrl_shift(), E),
            Command::ShowSearch => Shortcut::new(ctrl_shift(), F),
            Command::NextTab => Shortcut::new(CTRL, Tab),
            Command::PrevTab => Shortcut::new(ctrl_shift(), Tab),
            Command::OpenSettings => Shortcut::new(CTRL, Comma),
            Command::Shortcuts => Shortcut::new(CTRL, K),
            Command::Plugins => Shortcut::new(ctrl_shift(), X),
            _ => return None,
        })
    }

    /// Shortcut text shown in menus and the palette, formatted for display.
    pub fn shortcut_text(self) -> Option<String> {
        self.shortcut().map(|s| s.display())
    }

    /// Commands whose keys are handled by the editor canvas directly.
    pub fn is_editor_command(self) -> bool {
        matches!(
            self,
            Command::Undo
                | Command::Redo
                | Command::Cut
                | Command::Copy
                | Command::Paste
                | Command::SelectAll
                | Command::SelectNextOccurrence
                | Command::SelectAllOccurrences
                | Command::AddCursorUp
                | Command::AddCursorDown
                | Command::Find
                | Command::Replace
                | Command::FindNext
                | Command::FindPrev
                | Command::TriggerSuggest
                | Command::ToggleComment
                | Command::DuplicateLine
                | Command::DeleteLine
                | Command::MoveLineUp
                | Command::MoveLineDown
                | Command::Indent
                | Command::Outdent
                | Command::ToggleFold
                | Command::SelectExpand
                | Command::SelectShrink
                | Command::JumpToBracket
                | Command::JumpBack
                | Command::JumpForward
        )
    }

    /// Commands that the workbench still handles while a terminal owns keyboard focus.
    /// Every other key chord (Ctrl+W, Ctrl+P, Ctrl+N, Ctrl+B, Ctrl+K, Ctrl+S, etc.)
    /// passes straight through to the running shell / TUI application.
    pub fn skip_shell_in_terminal(self) -> bool {
        matches!(
            self,
            Command::ShowPalette
                | Command::ToggleTerminal
                | Command::FocusTerminal
                | Command::NewTerminal
                | Command::TerminalClose
                | Command::TerminalClear
                | Command::TerminalSplit
                | Command::TerminalUnsplit
                | Command::TerminalNext
                | Command::TerminalPrev
                | Command::TerminalMaximize
                | Command::ShowProblems
                | Command::ShowOutput
                | Command::OpenSettings
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_parse_roundtrips_every_builtin() {
        for command in Command::ALL {
            let Some(shortcut) = command.shortcut() else {
                continue;
            };
            assert_eq!(
                Shortcut::parse(&shortcut.display()),
                Some(shortcut),
                "{} {:?}",
                command.id(),
                shortcut
            );
        }
        assert_eq!(
            Shortcut::parse("ctrl+alt+u"),
            Some(Shortcut::new(ctrl_alt(), Key::U))
        );
        assert!(Shortcut::parse("ctrl+nope").is_none());
        assert_eq!(Shortcut::parse_list("ctrl+a, ctrl+b").len(), 2);
    }

    #[test]
    fn primary_modifier_matches_platform_command_key() {
        // macOS reports ⌘ through `command`/`mac_cmd`; Windows/Linux report Ctrl
        // through both `ctrl` and `command`. Both must match our primary modifier.
        let mac_cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        };
        assert!(mac_cmd.matches_exact(CTRL));
        assert!(Modifiers::COMMAND.matches_exact(CTRL));
    }

    #[test]
    fn command_ids_are_unique_and_roundtrip() {
        let mut ids: Vec<&str> = Command::ALL.iter().map(|command| command.id()).collect();
        for command in Command::ALL {
            assert!(!command.id().is_empty());
            assert_eq!(Command::from_id(command.id()), Some(*command));
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Command::ALL.len());
        assert_eq!(Command::from_id("file.save"), Some(Command::Save));
        assert_eq!(Command::from_id("plugins.manage"), Some(Command::Plugins));
        assert_eq!(Command::from_id("no.such"), None);
    }

    #[test]
    fn terminal_skip_shell_blocks_common_shell_chords() {
        // Common shell editing/navigation chords must pass through to the PTY.
        for cmd in [
            Command::CloseTab,      // Ctrl+W
            Command::QuickOpen,     // Ctrl+P
            Command::NewFile,       // Ctrl+N
            Command::ToggleSidebar, // Ctrl+B
            Command::Shortcuts,     // Ctrl+K
            Command::Save,          // Ctrl+S
            Command::OpenFile,      // Ctrl+O
            Command::GotoLine,      // Ctrl+G
            Command::ZoomIn,        // Ctrl++
            Command::ZoomOut,       // Ctrl+-
            Command::ZoomReset,     // Ctrl+0
        ] {
            assert!(
                !cmd.skip_shell_in_terminal(),
                "{} must not steal terminal keys",
                cmd.id()
            );
        }
        // Workbench terminal commands stay reachable while focused in a terminal.
        for cmd in [
            Command::ShowPalette,
            Command::ToggleTerminal,
            Command::FocusTerminal,
            Command::NewTerminal,
            Command::TerminalClose,
            Command::TerminalClear,
            Command::TerminalSplit,
        ] {
            assert!(
                cmd.skip_shell_in_terminal(),
                "{} should remain active in terminal",
                cmd.id()
            );
        }
    }
}
