<div align="center">

<img src="icons/app-icon.png" width="96" alt="ShiGuang" />

# ShiGuang 拾光

**A tray-resident AI desktop assistant for Windows**

Organize your desktop, remind you of todos, drive your browser, run your servers and record polished screen demos,<br/>
and when you need a new feature, it can write a plugin for itself.

[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6?logo=windows&logoColor=white)](#-getting-started)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app)
[![Rust](https://img.shields.io/badge/Rust-backend-CE422B?logo=rust&logoColor=white)](src-tauri)
[![React](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)](src)
[![Version](https://img.shields.io/badge/version-0.1.6-8B5CF6)](https://github.com/xuantiandaozun/shiguang/releases)
[![License](https://img.shields.io/badge/license-MIT-22C55E)](LICENSE)

**English** · [简体中文](README.zh-CN.md) · [Download](https://github.com/xuantiandaozun/shiguang/releases) · [Plugins](docs/PLUGINS.md) · [LINUX DO](https://linux.do)

</div>

> The app UI and most design docs are in Chinese.

---

## ✨ Highlights

- 🫧 **Floating ball + chat window**: lives in the tray, one click to talk, and closing the window keeps it running.
- 🗂️ **Desktop organizing**: you see the plan first and confirm before anything moves; it only moves files, never deletes, and every batch can be undone.
- 🌐 **Your own browser**: a companion extension reads, clicks and fills pages in the browser you are already logged into. A trusted-input engine waits for the page to settle before it reports what happened.
- 📸 **Screenshots**: drag a region, or click any app window to capture all of it (even when covered); global hotkey, copied to the clipboard automatically.
- 🎬 **Screen recording**: record web or desktop-app workflows; on export it zooms in on clicks and draws a smooth cursor. Sensitive data on web pages is blurred at the source, and you can cut clips, write narration, add a voiceover, background music and subtitles.
- 🧑‍💻 **Workspaces & code agents**: it remembers how your projects are verified and deployed, and can hand code changes to Claude Code / Codex, then review the result and commit with your consent.
- 🖥️ **Server ops**: SSH / SFTP, interactive terminals and conversational operations, with each action confirmed according to its risk.
- 🧩 **Plugins all the way down**: tools, prompts, skills and UI all come from plugins and take effect as soon as you edit the files. ShiGuang can also author plugins itself.

## 🆕 What's New

> Built after v0.1.6; ships with the next release.

- **🎬 Screen recording (new)**
  - **Capture**: records the current browser tab. It saves the raw frames plus an input-event track and applies effects at export time, so you can adjust them as often as you like.
  - **Auto-zoom**: zooms in on clicks and typing and follows smoothly; it redraws a larger cursor with click ripples. A "padded" style adds a gradient background and rounded corners for promo videos.
  - **Source-side masking**: passwords, phone numbers, ID cards, emails, bank cards and keys are blurred on the page itself, so the recorded frames never contain them. You can add your own selectors and regexes.
  - **Timeline editor**: cut clips, box extra masks, edit zoom segments and write narration, with a quick preview.
  - **Voiceover & subtitles**: ShiGuang calls a TTS CLI installed on your machine (for example Volcengine's) to generate the voiceover. If a line runs longer than the footage, the video freezes until it finishes. Subtitles are written as `.srt` and can also be drawn onto the video.
  - **Recorded by ShiGuang**: it can operate the page while recording and mark each step, and those marks become the narration draft.
- **🧩 Plugin architecture**: a thin Rust core plus plugins. Tools, prompts, skills and UI are all plugin files, so changes need no rebuild. A new Plugins tab lets you enable, disable and roll back plugins, or have ShiGuang write one.
- **🧑‍💻 Workspaces & code agents**: register your projects so you can refer to them by name; hand code changes to the Claude Code / Codex CLIs.
- **🖱️ Trusted browser input**: clicks, typing and uploads are sent as real input through the debugger protocol; after each action it waits for the page to settle, then reports what changed and any new tabs.

<details>
<summary>v0.1.4 – v0.1.6</summary>

- **Servers & AI ops**: SSH/SFTP connection manager, standalone terminals and conversational operations; remote browsing, text previews, multi-file uploads, transfer progress and Tab completion.
- **Task delegation**: Codex / Cursor can delegate work to ShiGuang over a local MCP server, with review checkpoints, multi-round corrections and resume.
- **Models & agents**: per-provider model catalogs and plan expiry dates; research, file and browser subagents are called automatically.
- **UI & releases**: refreshed main window, chat and floating ball; releases are built locally by default, with GitHub signing them and generating the update metadata.

See the [v0.1.5 release notes (Chinese)](docs/releases/0.1.5.md).

</details>

## 🧰 Features

### Everyday

| Feature | Description |
| --- | --- |
| 🗂️ Desktop organizer | "Organize my desktop" → classification plan → confirm in the chat card. Moves only, never deletes; each batch can be undone. The rules live in an editable skill |
| ⏰ Todos | Create them in natural language ("remind me at 3pm tomorrow to send the weekly report"); daily/weekly repeats, priorities and snooze; reminders arrive as notifications or popups, and you can reply to the AI from the popup |
| 🔎 File index | Persistent Everything-like metadata index for bulk search and whole-drive analysis, with a fast NTFS/MFT estimate |
| 📄 Files & vision | Read text and PDFs, local OCR (PaddleOCR) and image understanding; every file write is backed up first |

### Browser & content

| Feature | Description |
| --- | --- |
| 🌐 Browser control | Drives the browser you already use (with your logins) through the companion extension, failing over to a standalone CDP instance. Reads articles, takes snapshots, clicks, types and extracts data |
| 📸 Screenshots | Region, click-a-window, a named app or full screen; ShiGuang's own windows are left out unless you include them; saved to `Pictures/拾光截图` and copied to the clipboard |
| 🎬 Screen recording | Record a browser tab, a whole screen, an app window or a selected region (Ctrl+Alt+R), then export MP4 with auto-zoom, a synthetic cursor, masking, cuts, narration, voiceover, background music and subtitles. See [below](#-screen-recording) |
| 🔍 Web search | Search and fetch web pages |

### Dev & ops

| Feature | Description |
| --- | --- |
| 🧑‍💻 Workspaces & agents | Remembers each project's path, verify commands, deploy steps and rules; delegates code changes to Claude Code / Codex, reviews the result and commits with your consent. [Docs (Chinese)](docs/WORKSPACES_AGENTS.md) |
| 🖥️ Servers | SSH/SFTP connections, remote commands, browsing and transfers; an interactive terminal with Tab completion that can switch to conversational ops. [Docs (Chinese)](docs/SERVERS_MCP.md) |
| ⚙️ Background commands | Runs PowerShell, cmd or AI-written scripts; long jobs run in the background and the chat resumes when they finish; JSON output is parsed automatically |
| 🤝 Delegation MCP | Codex / Cursor can hand tasks to ShiGuang, with review, follow-ups and interruption. [Docs (Chinese)](docs/DELEGATION_MCP.md) |

### Extensibility & models

| Feature | Description |
| --- | --- |
| 🧩 Plugins | Tools, prompts, skills and UI all come from plugins; script, HTTP, command and MCP executors, plus declarative views and iframe pages. [Plugin guide](docs/PLUGINS.md) |
| 📚 Skills | Read-only built-in skills plus external skills the AI can create and refine; syncs with Claude / Codex / Cursor skill folders |
| 🤖 Models & usage | Save several model profiles (DeepSeek, Qwen, Volcengine Agent Plan, OpenAI, …) and switch between them; shows token usage and cache-hit rate |

## 🎬 Screen Recording

```
Capture (raw frames + event track)  →  Timeline (zoom / mask / cut / narration)  →  MP4 + SRT
```

1. **Record**: hit Start in the Recording tab, or tell ShiGuang "record a demo of creating a user in the admin panel" and it will operate the page while recording.
2. **Edit**: in the editor, scrub the timeline, box extra masks on the frame, adjust zoom segments, cut the waiting parts and write narration.
3. **Voice**: say "write narration for this recording and voice it with the Volcengine CLI". ShiGuang shows you the script first, then runs your local TTS CLI and attaches the audio to the timeline.
4. **Export**: start with a quick preview if you like; the final export writes an MP4 plus a matching `.srt`.

**Privacy**:
- Only the current tab is recorded.
- Sensitive content is blurred on the page itself.
- Raw footage stays on your machine.
- Only narration text goes to the TTS service, never video.
- Watch the result in full before you share it.

The first export needs ffmpeg: it uses the one already on your machine and only downloads a copy if there is none. You can also set the path in the plugin settings.

## 🧩 Plugin Architecture

```
Rust core (src-tauri/src)                 Plugins (src-tauri/plugins + user dir)
├─ windows, tray, IPC, updater             ├─ plugin.json    manifest, permissions, mounts, settings
├─ agent loop, LLM, context management     ├─ tools.json     tool definitions + executors
├─ files, commands, DB, browser bridge,    ├─ prompts/ skills/  prompts and internal skills
│  recording renderer                      └─ ui/ *.js       declarative views, pages, scripts, hooks
└─ plugin host (load, validate, hot reload, safe mode)
```

- Edit a plugin and it reloads. Built-in plugins are read-only; user plugins live in the app data folder.
- When ShiGuang writes a plugin, the change is validated, previewed for your confirmation and snapshotted before it takes effect, and it can be rolled back.
- Plugin pages run in a sandboxed iframe with no network access and can use only the capabilities they declare, through an SDK.
- After two failed launches in a row, the app starts in safe mode and loads built-in plugins only.

Docs: [PLUGINS.md](docs/PLUGINS.md) · [PLUGIN_GUIDE](src-tauri/plugins/core/PLUGIN_GUIDE.md) · [example plugin](docs/plugin-examples/quick-notes/)

## 🛡️ Safety by Design

- **Desktop organizing**: every run needs your confirmation; it moves files and never deletes them (deletes go to the recycle bin); shortcuts, `desktop.ini` and hidden files are left alone; every move is logged and can be undone.
- **Files & commands**: writes are backed up first; destructive commands need your consent; whole-drive scans ask first, and the index stores metadata only, never file contents.
- **Browser & recording**: network observation keeps only redacted request shapes; recordings are masked on the page, and keystrokes in password fields are never logged.
- **Code agents**: delegated agents can never commit or push; ShiGuang commits only after you confirm.
- **Servers**: you must verify host fingerprints on first connection and whenever they change. Passwords are stored in plaintext in the local database, so protect the data folder. During AI ops, targets, command output and local paths are sent to your configured model; file contents transferred over SFTP are not.

## 🚀 Getting Started

**Install**: download the installer from [Releases](https://github.com/xuantiandaozun/shiguang/releases). After the first launch:
1. Right-click the tray icon, open the main window and add a model profile under Settings.
2. Browser extension: open the extensions page in Chrome / Edge, turn on developer mode, click "Load unpacked" and choose the `browser-extension` folder in the install directory. After you upgrade ShiGuang, click "Reload" on the extensions page.

**From source** (requires Node.js 18+, Rust with the MSVC toolchain, Microsoft C++ Build Tools, and Windows 10/11):

```bash
npm install
npm run tauri:dev      # develop
npm run tauri:build    # build the EXE (plugins synced to target/release/plugins)
npm run tauri:bundle   # build the installer
```

Maintainers: see the [local release guide (Chinese)](docs/LOCAL_RELEASE.md).

> Windows notifications show the app's name and icon only for the **installed** app; otherwise they appear as coming from PowerShell. This is a Windows limitation.

## 📁 Data Location

`%APPDATA%/com.deskhelper.win/`

| Path | Contents |
| --- | --- |
| `deskhelper.db` | todos, operation logs, settings, chat history, workspaces |
| `file_index.db` | file metadata index (no file contents) |
| `plugins/` | user plugins and their version snapshots |
| `recordings/` | raw recordings and timelines (exports default to `Videos/拾光录屏`) |
| `skills/` · `tasks/` · `temp/` · `file_backups/` | external skills, background task logs, AI scratch files, write backups |
| `tools/` | tools downloaded on demand (e.g. ffmpeg) |

API keys stay in the local database and are only used to call the model providers you configure.

## 🗂️ Project Layout

```
src/                        React frontend
  windows/                  floating ball, chat, main window …
  components/               main-window tabs (Recording, Workspaces, Servers, Todos …)
  plugins/                  plugin mounts: registry, iframe host + SDK, declarative views, Plugins tab
  lib/ipc.ts                the single declaration of the IPC surface
src-tauri/src/              Rust core
  llm/                      agent loop, OpenAI-compatible client, tool execution, subagents
  plugins/                  plugin host (loader, executors, QuickJS, MCP, hooks, hot reload)
  browser/                  extension bridge + CDP, trusted-input engine
  recorder/                 recording: capture, timeline, renderer (tiny-skia), voiceover mix, ffmpeg
  organizer/ todo/ servers.rs workspaces.rs agents/ delegation.rs file_index.rs …
src-tauri/plugins/          18 built-in plugins (core, browser, recorder, workspaces …)
browser-extension/          companion extension (WebSocket bridge to the app)
docs/                       design and usage docs
```

## 🧭 Roadmap

- [x] Web screen recording: auto-zoom, source-side masking, cuts, voiceover, subtitles
- [ ] Desktop app recording: per-window capture, global mouse tracking, OCR masking, optional exclusion of ShiGuang's own windows
- [ ] Background music and keystroke overlays for recordings
- [ ] WeChat desktop chat-history monitoring (UIA / OCR approach and compliance risks under evaluation)

## 📄 License

[MIT](LICENSE)

## 💬 Community

This project endorses and links to the [LINUX DO community](https://linux.do), where it was first shared with fellow developers.
