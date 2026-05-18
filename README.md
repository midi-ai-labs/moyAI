<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>A local-first coding agent for private workspaces, local LLMs, and closed-network development.</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.1.0_beta-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.ja.md">日本語 README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0">Download beta</a>
  ·
  <a href="#quick-start">Quick Start</a>
  ·
  <a href="#configuration">Configuration</a>
</p>

---

## What Is moyAI?

moyAI is a Rust-based coding agent built for environments where cloud-first developer tools are hard to adopt.

It connects to an OpenAI-compatible local LLM server, reads and edits your workspace, runs shell commands, keeps session history, and presents the same agent core through a CLI, TUI, and Tauri Desktop app.

The focus is straightforward: keep the model local, keep the evidence visible, and keep the workflow useful for real engineering tasks.

## Why It Exists

Many coding agents assume hosted models, online services, plugin marketplaces, and constant internet access. That is not always realistic for private source code, internal networks, local inference servers, or reproducible engineering environments.

moyAI is designed around those constraints:

| Principle | What It Means |
| --- | --- |
| Local-first | Works with OpenAI-compatible local LLM endpoints such as LM Studio. |
| Workspace-aware | Searches, reads, edits, patches, and verifies files in your project. |
| Evidence-oriented | Keeps transcript, file changes, tool output, and session history inspectable. |
| GUI and terminal | Offers Desktop, CLI, and TUI entrypoints over the same Rust core. |
| Closed-network friendly | Release builds run without npm, Rust toolchain, internet, or a dev server on the target machine. |

## Highlights

- Tauri Desktop app with project chat, quick chat, transcript, artifacts, settings, and provider discovery.
- CLI and TUI for terminal-centered workflows.
- OpenAI-compatible local LLM connection with model availability checks.
- LM Studio metadata discovery through `/v1/models` and `/api/v1/models`.
- Workspace search, directory inspection, guarded file reads, diff-based edits, and shell execution.
- Permission presets: `default`, `auto_review`, and `full_access`.
- Vision-capable model support for image attachments.
- Optional Docling Serve and HTTP MCP integration for document-heavy workflows.
- Local instructions from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md`, and local `SKILL.md` files.
- Protocol-first session history with Markdown export and deterministic preflight / harness commands.

## Current Release

The first beta release is available here:

[**moyAI v0.1.0 beta release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0)

The Windows release zip includes:

- `bin/moyai.exe` for CLI / TUI workflows
- `bin/moyai-desktop.exe` for the Desktop app
- bundled `ui/desktop-web/dist/` assets
- README files, license, release notes, config example, manifest, and SHA256 checksums

On the target Windows machine, you do not need npm, the Rust toolchain, internet access, or a local web dev server.

## Quick Start

1. Start a local OpenAI-compatible LLM server.
2. Download and extract the latest release zip.
3. Launch `bin/moyai-desktop.exe`.
4. Open `LLM URL`, set the base URL and model, then confirm model discovery.
5. Use Quick Chat, or select a project workspace and start a development chat.

CLI examples:

```bash
moyai run --dir /path/to/workspace "Inspect this project and summarize the main modules."
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

Development build:

```bash
cargo build
```

Desktop release build:

```bash
npm install
npm run build:desktop-web
cargo build --release --bin moyai --bin moyai-desktop
```

Windows release package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 0.1.0
```

By default, release artifacts are written outside the repository under `project_sandbox/releases/`.

## Configuration

moyAI uses one user-wide config file, then applies environment variables and CLI overrides on top.

Default Windows config path:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

The release folder and workspace folders do not need their own config file. Desktop, TUI, and CLI all read the same user-wide settings.

Example:

```toml
[model]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-35b-a3b"
context_window = 131072
supports_tools = true
supports_images = true

[permissions]
access_mode = "auto_review"

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"

[mcp]
enabled = false
```

Common environment variables:

- `MOYAI_BASE_URL`
- `MOYAI_MODEL`
- `MOYAI_CONFIG_PATH`
- `MOYAI_DATA_DIR`
- `MOYAI_ACCESS_MODE`
- `MOYAI_REQUEST_TIMEOUT_MS`
- `MOYAI_STREAM_IDLE_TIMEOUT_MS`
- `MOYAI_CONTEXT_WINDOW`
- `MOYAI_MAX_OUTPUT_TOKENS`
- `MOYAI_SUPPORTS_IMAGES`
- `MOYAI_DOCLING_ENABLED`
- `MOYAI_MCP_ENABLED`

## Startup Checks

On cold start, `moyai-desktop.exe` shows the moyAI splash for at least five seconds while it checks:

- global config file state
- workspace availability
- configured provider and model catalog
- Docling Serve `/health` and `/ready` when Docling is enabled

If setup attention is needed, the app opens directly to Settings or LLM URL.

## Project Instructions

moyAI loads local project instructions from:

- `AGENTS.md`
- `CLAUDE.md`
- `.moyai/rules`
- `.moyai/rules-<route>`
- `.moyai/commands/*.md`
- `.moyai/skills/**/SKILL.md`

This keeps project behavior local to the repository and avoids depending on an external plugin marketplace.

## Verification

Useful local checks:

```bash
cargo fmt --all --check
cargo check
cargo test --lib
cargo test --tests
cargo run --bin moyai -- preflight run
```

## Status

moyAI is currently developed and tested primarily on Windows. The main development profile uses `qwen/qwen3.6-35b-a3b` hosted by LM Studio, especially the `lmstudio-community` build.

Other OpenAI-compatible models can be used, but model behavior, tool-use quality, context length, and vision support vary by provider and model.

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
