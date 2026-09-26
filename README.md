<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center"><strong>A coding agent for local LLMs and private networks.</strong></p>

<p align="center">
  <a href="README.ja.md">日本語</a> ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v2.1.1">Published v2.1.1</a> ·
  <a href="#quick-start">Quick Start</a> ·
  <a href="LICENSE">MIT License</a>
</p>

<p align="center"><img src="logo/moyai-screenshot-sample.png" alt="moyAI Desktop screenshot" width="920"></p>

## What Is moyAI?

moyAI is a Rust coding agent with Desktop, CLI and TUI interfaces. It connects to an external OpenAI-compatible LLM server, reads projects, edits files, runs commands and records conversations and results. It does not start, stop or download the LLM server or its models.

**This branch develops Desktop 3.0.0 with moyAI Hub 0.1.0. It is not a published release.** The published v2.1.1 ZIP has its original layout and behavior. See the [development release notes](docs/release/v3.0.0.md) for implemented changes and remaining validation.

## Highlights

- Local projects and quick chats, file attachments, visible tool activity, outputs and saved conversation history.
- A separate, tool-free Side Chat for questions about the main conversation and selected content.
- Three operation modes: ask for approval, AI review of approvals, and full access. Windows workspace restrictions apply in the first two modes; see [permission limits](#permissions-and-platform-limits).
- Recursive subagents, progress plans, long-conversation compaction and Markdown conversation export.
- Local project instructions and Skills; optional external HTTP MCP tools and Docling for documents.
- Hub projects for work across approved PCs. The normal project list marks them with `MCP`; the same chat interface accepts the request. The AI can select among the project's permitted execution PCs.
- A Windows Runner that executes jobs independently of the Desktop window. Hub stores shared conversations, inputs and published results.

## Quick Start

For a current development Windows package:

1. Extract the whole ZIP and run `Setup-moyAI.cmd`. Open **moyAI** from the Start menu. Portable use starts with `Start-moyAI.cmd`.
2. Choose your role in the first-use screen:

   | Purpose | Entry |
   | --- | --- |
   | Work on this PC | 自分のPCで使う |
   | Send requests to a team project | チームに参加する |
   | Let this PC execute team work | チームの仕事をこのPCで実行する |
   | Prepare a Hub | チーム環境を用意する |

3. For personal use, enter your AI endpoint and model, select the approval mode and save. Add a project folder or start a chat.
4. For team use, open the administrator's `hub-config.moyai-join`. The administrator approves the PC and assigns its project roles. Each execution PC also permits execution and registers a local folder for each project. A PC that only sends requests needs neither a local model nor an execution folder.
5. Open the project and send a normal request. Review any approval request, the answer and the output files.

Team access does not require a moyAI username/password. Every PC has its own ID and key, even when Windows usernames match. Hub membership alone does not grant access to every project.

Closing Desktop leaves it in the system tray; launching it again shows the existing window. Use **終了** in the tray menu to exit. Runner and Hub have their own lifetimes.

See [getting started](docs/user/getting-started.md), [Windows installation and updates](docs/user/windows-setup.md), [first-use recovery](docs/desktop-first-use.md), and [Hub project operation](docs/shared-work-desktop.md).

The published v2.1.1 ZIP uses `bin/moyai-desktop.exe`; it does not use the new Setup layout.

## Configuration

The user-wide Windows configuration is `%APPDATA%\midi-ai-labs\moyai\config\config.toml`. Existing user settings take precedence over defaults.

Use **設定 → AIの接続** for Main and Side Chat. Without Hub configuration, enter the endpoint and connection type: LM Studio Responses or OpenAI-compatible Chat Completions, such as oMLX. Load the model list or enter a model ID. API credentials, when required, are referenced by environment-variable name rather than saved as secret text.

When Hub is configured, this same screen shows Hub-managed connection information and lets you select the standard or another registered model. Main and Side selections are independent. Shared Runner jobs use the executing PC's Main selection. Hub outages do not silently switch to manual settings. **接続設定をリセット** works without a Hub response, preserves local conversations/files and manual AI settings, and lets you join a replacement Hub.

Global settings apply to future work; session settings can override the selected local Main conversation. Side Chat has its own global connection. `context_window` controls moyAI's local accounting and compaction; model loading, sampling and generation output limits belong to the LLM host. See [config.example.toml](config.example.toml) and [Hub model integration](docs/hub-integration.md).

## CLI and TUI

```bash
moyai run --dir /path/to/workspace "Inspect this project and summarize its main modules."
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai model availability --base-url http://omlx-host:8119/v1 --provider-profile openai_compatible
```

Installed development packages keep binaries in `app/bin/`. CLI, TUI and Desktop share the Rust core and user configuration. Use `moyai --help` and command-specific help for the current options.

## Project Instructions

Project instructions can come from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md` and local `SKILL.md` files. Files and tools remain subject to the selected workspace and permission mode. Existing files are the source of project state; rejoining a Hub project does not need a special history-restoration procedure.

External MCP tools remain configurable. The retired manual moyAI-to-moyAI publishing workflow is not the setup route for new shared work; use Hub projects. Saved legacy records remain available through [MCP history](docs/mcp-history-guide.md).

## Permissions and Platform Limits

Windows x64 is the primary supported and verified deployment. Distribution needs WebView2 and Visual C++ runtimes, either bundled or installed by the organization. Setup does not download prerequisites; target PCs do not need Rust, Node or a development server.

**Ask for approval** and **Approve for me** share Windows workspace restrictions; the latter uses an AI reviewer for permission decisions. **Full access** runs approved processes with the current Windows user's authority. These restrictions are not a complete OS isolation boundary. Native process sandboxing is Windows-only; restricted process operations fail closed elsewhere. Unix updates/deletes may return a partial-commit error with a preserved backup, which must be reviewed.

Stop and editing a request do not undo files already written or applications already started. Only the latest user message is editable; local messages with images and Hub messages with attached inputs are excluded from text-only edit/resend. A shared job's accepted cancellation is not proof that its process has stopped.

Runner operates while its Windows user is logged in. Windows service operation, automatic Hub root-CA replacement, and fresh-PC/physical multi-PC release acceptance are not implied by local automated tests. Model quality and tool support depend on the selected provider. See [shared Runner operation](docs/runner-shared.md), [local Runner operation](docs/runner-local.md) and [managed commands](docs/managed-shell-guide.md).

## Development and Verification

```bash
npm ci
npm run build:desktop-web
cargo build
```

Relevant checks:

```bash
cargo fmt --all -- --check
cargo check --all-features
cargo test -- --test-threads=1
npm run test:desktop-web
npm run verify:gui -- --suite smoke
```

Use the [GUI automation guide](tests/desktop_e2e/GUI_AUTOMATION.md) and [manual test entrypoint](tests/manual_ST/README.md) to select checks for the changed behavior. Real-window evidence and Live LLM behavior must be distinguished from fixture tests. Source ownership is visible in [src/](src/) and [the multi-PC design](docs/design/multi-device-session.md); implementation details belong with their code and tests rather than being duplicated here.

Release packaging uses `scripts/package-release.ps1` from a clean release commit. It rebuilds Desktop, CLI, Runner and cleanup, requires version/commit-specific manual GUI evidence, and checks the exact Hub binary when bundled. Runtime inputs and commands are in [Windows packaging](docs/user/windows-setup.md#配布を作る担当者向け). Publish the ZIP with its manifest and SHA256 sidecar only after the packaged applications pass acceptance.

## License

[MIT](LICENSE). Copyright (c) 2026 Hideyoshi Takahashi. `midi-ai-labs` is this personal project's GitHub namespace.
