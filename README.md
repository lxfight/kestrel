# Kestrel

轻量、极速、现代的原生 Rust 代码编辑器。基于 GPU 渲染（`eframe` / `egui` + `wgpu`）、Rope 文本缓冲（`ropey`）、增量 `tree-sitter` 语法分析引擎、原生 Git 集成、多模态文件查看器、多会话内置终端与内嵌 Lua 插件系统构建，拥有低常驻内存与接近瞬时的启动速度。

---

## 核心特性

### 高性能核心编辑器
- **Rope 缓冲与大文件保护**：基于 `ropey` 实现高效编辑；超过 8 MB 的文件自动以只读模式打开并关闭语法高亮与小地图；二进制文件自动启用只读保护。
- **多编码与行尾支持**：自动识别并无损保存 `UTF-8`、`UTF-8 with BOM`、`UTF-16 LE/BE`、`ANSI/GBK`，命令面板支持以指定编码重开/另存及 `LF`/`CRLF` 切换。
- **保存安全与会话恢复**：保存前校验磁盘 `mtime`，检测到外部修改时立即停止并提示防止误覆盖；支持自动保存（不清空撤销栈）以及崩溃/重启后的未命名缓冲区与工作台会话恢复。
- **原生 IME 支持**：完整支持中/日/韩输入法内联组合文本与候选窗跟随，编辑区与内置终端均可流畅输入中文。
- **多光标与高效编辑**：支持 `Alt+Click` 增删光标、`Ctrl+D` 增量选中、`Ctrl+Shift+L` 全选匹配、`Ctrl+Alt+↑/↓` 纵向多光标、`Alt+拖拽` 列选；提供自动闭合括号/引号、括号匹配高亮与跳转、选区智能扩展/收缩、拖拽移动或复制选区、跳转历史回退与前进。
- **现代视图能力**：代码折叠、粘性滚动（Sticky Scroll）、自动换行（Soft Word Wrap）、按语法着色的增强小地图（叠加当前行、选区、查找命中与 Git 变更）。

### 代码智能与多语言支持
- **内置 17 种语言语法引擎**：基于 `tree-sitter` 增量解析（Rust、Python、JavaScript、TypeScript、TSX、JSON、TOML、C、C++、Go、Java、C#、HTML、CSS、Shell/Bash、YAML）与独立 Markdown 语法扫描器。
- **补全与导航**：实时上下文与成员补全（输入 `.` / `::` 自动触发，`Ctrl+Space` 手动触发）；可折叠符号大纲（Outline）与面包屑层级定位；`F12` / `Ctrl+Click` 跨文件转到定义；语法诊断波浪线与悬停提示；`Shift+Alt+F` 格式化文档。

### 工作台与视觉体验
- **双主题与可读性设计**：内置深色（Dark）与浅色（Light）双主题，正文、注释与辅助文字均满足 WCAG AA（≥ 4.5:1）对比度；内置 VSCode Codicons 矢量图标库与按语言/目录语义着色的文件树图标；跨平台 CJK 字体回退链。
- **灵活布局**：支持向右拆分编辑器与拖拽标签/文件至边缘分屏，中缝可拖动调宽；标签栏支持溢出滚动、中键关闭、双击新建、固定与右键菜单；侧栏区块高度可拖动调整。
- **高效检索与配置**：命令面板按分类分组并置顶最近使用项；快速打开支持 `文件:行号` 直达；工作区多线程全局搜索与替换（支持仅文件名模式、正则与 `$1` 捕获组、include/exclude 持久化、单条替换与历史记录）；设置面板支持分区导航与搜索，快捷键面板支持实时录制改键。

### 原生 Git 集成与可视化 Diff
- **源代码管理（SCM）**：单文件/分组暂存、取消暂存、放弃更改（全量丢弃二次确认）；多行提交、`Ctrl+Enter` 提交、`Amend` 修订与撤销上次提交；分支过滤/切换/新建/合并/删除；`fetch` / `pull` / `push` / 发布分支；储藏（Stash）管理；冲突文件一键采用当前/对方版本；提交历史分页加载与历史版本只读浏览。
- **实时状态感知**：编辑器 Gutter 行内 Diff 指示条（新增/修改/删除），文件树、标签页与状态栏同步展示变更标记及分支 `↑ahead ↓behind`。
- **增强型 Diff 视图**：支持并排与内联双模式、语法高亮与词级差异；支持视图内文本选中与复制、查找匹配（`Ctrl+F`）、工作区行内直接编辑、Diff 小地图与概览标尺、横向滚动，以及按 Hunk 逐块暂存 / 取消暂存 / 丢弃。

### Markdown 与网页预览
- **三模式实时联动**：支持纯源码、纯预览与左右分屏三种模式（`Ctrl+Shift+V` / `Alt+V`），分屏模式下源码与渲染视图实时双向滚动同步。
- **完整 GFM 与扩展语法**：表格、任务列表、代码高亮与一键复制、Callout 提示框、脚注、数学公式、定义列表，以及常见 HTML 容器与内联样式。
- **远程图片、SVG 与浏览器预览**：内置异步远程图片加载与本地临时缓存；内置轻量原生 SVG 渲染引擎，无需外部浏览器即可直接呈现徽标与矢量图表；支持导出独立 HTML5 页面并在系统默认浏览器中预览。

### 多模态文件查看器
后台线程异步解析，避免阻塞 UI 渲染：
- **图片**：PNG、JPG/JPEG、GIF、WebP、BMP、ICO、TIFF
- **办公与文档**：PDF（Windows WinRT 渲染）、Word（`.docx`）、Excel（`.xlsx` / `.xlsm`，支持冻结表头）
- **结构化数据**：CSV / TSV 表格视图，支持与源码视图一键切换
- **压缩归档**：ZIP / JAR / WHL / APK / NUPKG / VSIX / XPI 目录浏览，支持双击解压查看归档内文件

### 多会话内置终端与底部面板
- 基于 `portable-pty` + `vt100`，自动探测 PowerShell / PowerShell 7 / CMD / Git Bash / WSL。
- 支持多终端实例切换与左右分屏双 PTY、5,000 行回滚缓冲、鼠标选中与 `Ctrl+Shift+C/V` 复制粘贴、回滚缓冲搜索、`Ctrl+Click` 打开链接、拖入文件插入路径，以及可选跟随活动文件目录。
- 底部面板统一集成 **终端（Terminal）**、**问题（Problems）** 与 **输出（Output）** 三大视图。

### Lua 插件系统
- 清单驱动（`plugin.json`）+ 内嵌 Lua 5.4（`mlua` vendored），在沙箱环境中运行（无 `io`/`os`/`package`/`debug` 库，单次回调限制 500 万条指令）。
- 支持注册命令与快捷键、状态栏交互项、文档读写、生命周期钩子、异步外部进程调用与插件私有配置持久化。
- 插件视图（`Ctrl+Shift+X`）支持免重启热启用、禁用、重载与重新扫描，实时展示各插件 Lua 内存占用并在禁用时立即回收。

---

## 技术栈与项目结构

| 领域 | 核心依赖 |
| --- | --- |
| GUI 与渲染 | `eframe` 0.37、`egui` 0.37（`wgpu` 后端） |
| 文本与检索 | `ropey`、`regex`、`ignore`、`notify` |
| 语法引擎 | `tree-sitter` 0.27 及 15 个语言语法包（覆盖 16 种代码语言） |
| 终端与插件 | `portable-pty`、`vt100`、`mlua`（Lua 5.4 vendored） |
| 多模态解析 | `image`、`flate2`、`roxmltree`、`windows`（WinRT PDF） |

```text
src/
├── main.rs            # 入口与窗口配置
├── app.rs             # 应用主状态、事件调度与工作台编排
├── commands.rs        # 命令注册表与快捷键解析
├── doc.rs             # Rope 文档模型、编码识别、保存冲突校验与撤销栈
├── editor/            # 编辑器视图、输入/IME、语法缓存、折叠、小地图
├── highlight/         # tree-sitter 增量高亮与 Markdown 词法扫描
├── lsp.rs             # 符号大纲、代码折叠、语法诊断与补全/跳转辅助
├── diff.rs            # 可视化 Diff 视图（并排/内联、行内编辑、Hunk 操作、小地图）
├── git.rs             # 原生 Git 命令封装、后台防抖扫描与写操作队列
├── markdown/          # Markdown/HTML 解析、egui 渲染、远程图片、原生 SVG、HTML 导出
├── viewer/            # 图片、PDF、DOCX、XLSX/CSV、ZIP 归档查看器
├── workspace/         # 文件树、后台文件索引、多线程全局搜索、文件监听
├── terminal.rs        # ConPTY / PTY 会话管理与 vt100 终端缓冲
├── plugin/            # Lua 5.4 插件宿主、清单解析、API 绑定与插件管理面板
├── ui/                # 顶栏、侧边栏、命令面板、弹窗、终端面板与基础控件
├── settings.rs        # 配置、自定义快捷键与会话状态持久化
├── theme.rs           # 深色/浅色设计系统 Token
├── fonts.rs           # 字体与 CJK 回退链加载
├── icons.rs           # Codicons 与文件/目录矢量图标绘制
└── fuzzy.rs           # 模糊匹配打分器
```

---

## 构建与运行

### 环境要求
- Rust 工具链（支持 **Rust 2024 Edition**，建议使用最新 stable 版本）
- 推荐平台：Windows x64（PDF 预览依赖 Windows WinRT API；其余模块基于跨平台 crate 构建）

### 从源码构建

```bash
# Release 构建并运行
cargo build --release
./target/release/kestrel

# 或直接运行
cargo run --release

# 运行单元测试
cargo test
```

Windows 下也可以使用仓库自带的构建脚本（会构建 Release 并将可执行文件复制到仓库根目录）：

```bash
bash scripts/build.sh          # Release 构建 -> ./Kestrel.exe
bash scripts/build.sh --debug  # Debug 构建
```

### 常用环境变量

| 变量 | 说明 |
| --- | --- |
| `KESTREL_OPEN=<path>` | 启动时直接打开指定文件或目录 |
| `KESTREL_THEME=dark\|light` | 覆盖启动主题 |
| `KESTREL_WINDOW=<W>x<H>` | 覆盖初始窗口尺寸（如 `1280x800`） |
| `KESTREL_PLUGINS=<dir>` | 指定自定义插件根目录（便于本地插件开发调试） |
| `KESTREL_TRACE=1` | 在输出面板记录插件加载、内存与命令耗时日志 |
| `KESTREL_PROFILE=1` | 输出启动与帧耗时诊断信息 |

### 配置与数据目录
- **Windows**：`%APPDATA%\Kestrel\`
- **macOS**：`~/Library/Application Support/Kestrel/`
- **Linux**：`$XDG_CONFIG_HOME/kestrel/` 或 `~/.config/kestrel/`

包含配置文件 `settings.json`、自定义快捷键 `keybindings.json`、会话恢复数据与默认插件目录 `plugins/`。

---

## 插件开发速览

在插件目录下新建文件夹 `<插件id>/`，放入 `plugin.json` 与 `main.lua`：

```json
{
  "id": "hello",
  "name": "Hello Plugin",
  "version": "0.1.0",
  "description": "示例插件",
  "apiVersion": 1,
  "main": "main.lua",
  "events": ["changed"]
}
```

```lua
kestrel.command("hello.greet", "向当前文档问好", function()
  local doc = kestrel.active_doc()
  if not doc then
    kestrel.notify("当前没有打开的文档", "warning")
    return
  end
  kestrel.notify("当前文件：" .. (doc:path() or "未命名"), "info")
end, "ctrl+alt+h")

kestrel.on("saved", function(ev)
  kestrel.log("已保存: " .. (ev.doc:path() or ""))
end)
```

常用宿主 API（全局表 `kestrel`）：
- **交互与命令**：`kestrel.command(id, title, fn, key?)`、`kestrel.run_command(id)`、`kestrel.status_item(key, text, command_id?)`、`kestrel.notify(text, level?)`、`kestrel.log(text)`
- **事件订阅**：`kestrel.on(event, fn)`，支持 `startup`、`opened`、`before_save`、`saved`、`closed`、`changed`（需在清单 `events` 声明）、`shutdown`
- **文档与工作区**：`kestrel.active_doc()`（提供 `path`、`language`、`is_dirty`、`text`、`text_range`、`line`、`line_count`、`caret`、`selection`、`set_text`、`insert`、`replace`、`set_selection`、`save`）、`kestrel.root()`、`kestrel.open_file(path)`、`kestrel.read_file(path)` / `kestrel.write_file(path, text)`
- **扩展与持久化**：`kestrel.exec{ program, args?, stdin?, on_done }`（异步执行外部进程）、`kestrel.config(key, default)` / `kestrel.set_config(key, value)`

在编辑器中按 `Ctrl+Shift+X` 打开插件面板，即可随时「重新扫描」或「重载」调试中的插件。

---

## 常用快捷键

> 以下为 Windows / Linux 默认快捷键；macOS 下 `Ctrl` 自动对应 `Cmd`。可在快捷键面板（`Ctrl+K`）中检索并直接录制修改。

### 导航与工作台
| 操作 | 快捷键 |
| --- | --- |
| 快速打开文件（支持 `文件:行号`） | `Ctrl+P` |
| 命令面板 | `Ctrl+Shift+P` |
| 切换侧边栏 | `Ctrl+B` |
| 资源管理器 / 全局搜索 / 插件视图 | `Ctrl+Shift+E` / `Ctrl+Shift+F` / `Ctrl+Shift+X` |
| 切换底部终端 / 新建终端 | `Ctrl+J` / `` Ctrl+Shift+` `` |
| 问题面板 / 输出面板 | `Ctrl+Shift+M` / `Ctrl+Shift+U` |
| 向右拆分编辑器（分屏） | `Ctrl+\` |
| 切换下一个 / 上一个标签页 | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| 偏好设置 / 快捷键总览 | `Ctrl+,` / `Ctrl+K` |
| 放大 / 缩小 / 重置界面缩放 | `Ctrl++` / `Ctrl+-` / `Ctrl+0` |

### 编辑与代码导航
| 操作 | 快捷键 |
| --- | --- |
| 查找 / 替换 | `Ctrl+F` / `Ctrl+H` |
| 跳转到行 / 跳转到符号（Outline） | `Ctrl+G` / `Ctrl+Shift+.` |
| 转到定义 | `F12` / `Ctrl+Click` |
| 触发代码补全 / 格式化文档 | `Ctrl+Space` / `Shift+Alt+F` |
| 选中下一个相同词 / 全部相同词 | `Ctrl+D` / `Ctrl+Shift+L` |
| 上/下方添加光标 / 任意位置添加光标 | `Ctrl+Alt+↑` / `Ctrl+Alt+↓` / `Alt+Click` |
| 扩展 / 收缩选区 | `Shift+Alt+→` / `Shift+Alt+←` |
| 跳转匹配括号 / 切换代码折叠 | `Ctrl+Shift+\` / `Ctrl+Shift+[` |
| 切换注释 / 复制行 / 删除行 | `Ctrl+/` / `Ctrl+Shift+D` / `Ctrl+Shift+K` |
| 上移 / 下移当前行 | `Alt+↑` / `Alt+↓` |
| 跳转历史后退 / 前进 | `Alt+←` / `Alt+→` |
| 切换自动换行 / 切换小地图 | `Alt+Z` / `Alt+M` |
| Markdown 预览 / 侧边分屏预览 | `Ctrl+Shift+V` / `Alt+V` |

---

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
