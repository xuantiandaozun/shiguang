# ShiGuang (拾光) — AI Desktop Assistant for Windows

**English | [简体中文](README.zh-CN.md)**

A tray-resident AI desktop assistant: a floating ball + chat window backed by an LLM that **organizes your desktop**, **reminds you of todos on time**, and **operates your browser**.

## Features

| Module | Description |
| --- | --- |
| AI Desktop Organizer | Say "organize my desktop" → get a classification plan → confirm in the chat card to execute. Moves only, never deletes; one-click undo per batch |
| Automation Rules | After confirming a plan, tell the AI "use this rule from now on", or create rules manually; new desktop files matching a rule are filed automatically (only approved rules run; global pause available) |
| Todos | Natural-language creation ("remind me to submit the weekly report at 3pm tomorrow"); daily/weekly repeats, priorities, snooze; reminder via Windows notification or popup — the input popup lets you reply to the AI right from the reminder |
| Browser Control | Read article bodies (Readability), snapshot and click/type on pages through the companion extension (your real browser, with your logins), with automatic failover to a standalone CDP instance |
| Background Commands | The AI can run shell commands in the background (builds, watchers) and check on them later; can be disabled in Settings |
| Agent Skills | Built-in read-only skills + external skills the AI can create and refine; syncs with Claude / Codex / Cursor skill folders |
| File & Vision Tools | Read files (text / PDF), OCR, image understanding, safe writes with automatic backups, read-only subagents |
| Floating Ball | Always-on draggable orb; click to toggle the chat window; closing windows keeps the app running in the tray |
| Main Window | Todos / Rules / History (undo) / Background Tasks / Skills / Settings |

## Safety by Design

- Every organizing execution requires manual confirmation (plan card in the chat panel)
- Moves only, never deletes (delete = recycle bin); name clashes get a `(n)` suffix
- Shortcuts `.lnk/.url`, `desktop.ini`, and hidden files are untouched by default
- Every move is logged to SQLite `operation_logs`; undo = reverse move
- File writes are auto-backed up; destructive shell commands require explicit consent

## Tech Stack

Tauri 2 (Rust) + React 18 + TypeScript + Vite + TailwindCSS + Zustand + SQLite (rusqlite bundled).
LLM via OpenAI-compatible APIs (DeepSeek / Qwen / OpenAI …), Function Calling + SSE streaming.

## Requirements

- Node.js 18+
- Rust (rustup, MSVC toolchain) + Microsoft C++ Build Tools (required to compile bundled SQLite)
- Windows 10/11 (WebView2 built in)

## Run Locally

```bash
npm install
npm run tauri:dev
```

First run: right-click the tray icon → open the main window → Settings → fill in your LLM API (DeepSeek / Qwen / OpenAI presets), save, then chat.

## Build

```bash
npm run tauri:build
```

Output: NSIS installer under `src-tauri/target/release/bundle/nsis/`.

> Note: Windows notifications show the app name and icon only for the **installed** app; otherwise they appear as coming from PowerShell — a system limitation.

## Data Location

`%APPDATA%/com.deskhelper.win/`:

- `deskhelper.db` — SQLite: todos, rules, operation logs, settings, chat history
- `tasks/`, `skills/`, `screenshots/`, `temp/` (AI scratch files), `file_backups/`

API keys live only in this local database and are never uploaded.

## Directory Layout

```
src/                    React frontend
  windows/              FloatBall / ChatPanel / Reminder / MainWindow pages
  components/           Main window tabs (Todos / Rules / History / Tasks / Skills / Settings)
  lib/ipc.ts            Sole declaration of the invoke/event IPC surface
  stores/chat.ts        Chat state (zustand)
src-tauri/src/
  lib.rs                Assembly: plugins, tray, background tasks
  commands.rs           IPC command layer (settings stored in SQLite)
  db.rs                 All SQLite reads/writes
  llm/                  OpenAI-compatible client (SSE + Function Calling), agent loop,
                        tool set, subagent, prompts, profile, vision
  organizer/            Desktop scanner, plan executor/undo, rule engine, file watcher
  browser/              Extension bridge + CDP; page-api + Readability (browser_read)
  todo/scheduler.rs     Todo reminder scheduler (notify / popup / popup-with-input)
  tasks.rs              Background commands;  skills.rs + builtin_skills.rs  Agent Skills
  reader.rs / writer.rs / ocr.rs / machine.rs / tempfs.rs
src-tauri/builtin-skills/  Built-in skill sources (compiled into the app)
browser-extension/      Companion browser extension (WebSocket bridge to the app)
scripts/gen-icon.cjs    Icon generator (node scripts/gen-icon.cjs)
```

## Roadmap

- WeChat desktop chat-history monitoring (UIA / OCR route and compliance risks under evaluation)

## License

[MIT](LICENSE)
