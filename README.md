<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="640">
</p>

# moyAI

**moyAI** is a local-first coding agent for developers who need practical software engineering support in restricted, private, or offline-friendly environments.

It connects to OpenAI-compatible local LLM servers, works directly with your workspace, and provides CLI, TUI, and native desktop interfaces on top of the same Rust core.

[日本語版 README](README.ja.md)

## Why moyAI

Many coding agents assume a cloud-first environment: network access, hosted models, online plugin ecosystems, and external services. That model is not a good fit for teams working with private code, closed networks, local inference servers, or reproducible internal tooling.

moyAI was built for that environment. It focuses on local execution, explicit configuration, file-system-aware tools, durable session history, and deterministic verification assets that help you understand what happened during an agent run.

## Features

- Local LLM support through OpenAI-compatible APIs.
- LM Studio model discovery through `/v1/models` and `/api/v1/models` metadata.
- CLI, TUI, and native Slint desktop app.
- Workspace search, directory inspection, file reading, diff-based editing, and shell execution.
- Size-aware read guards for large files, binary files, model checkpoints, and structured documents.
- Optional Docling Serve and HTTP MCP integration for closed-network document workflows.
- Session persistence with protocol-first history and Markdown export.
- Vision-capable model support for image attachments from CLI and desktop workflows.
- Permission presets: `default`, `auto_review`, and `full_access`.
- Local instruction loading from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, and local `SKILL.md` files.
- Reusable workflow commands from `.moyai/commands/*.md`.
- Review entrypoints for uncommitted changes and branch comparison.
- Deterministic preflight and harness commands for validating runtime contracts.

## Project Status

moyAI is an active Rust implementation aimed at closed-network and local-LLM use. The current repository includes the core runtime, CLI, TUI, desktop app, session storage, tool execution layer, provider metadata probing, and harness/preflight infrastructure.

## Current Target Environment

moyAI is currently developed and tested primarily on Windows.

During development, moyAI has been optimized for `qwen/qwen3.6-35b-a3b` hosted by LM Studio, specifically the `lmstudio-community` build. Other models and broader provider profiles are planned for future development.

## Requirements

- Rust toolchain with the 2024 edition support.
- A local or reachable OpenAI-compatible LLM endpoint.
- Optional: LM Studio for model metadata discovery.
- Optional: Docling Serve or configured HTTP MCP servers for document workflows.

## Build

```bash
cargo build
```

## Run

```bash
cargo run -- run --dir /path/to/workspace "Inspect this project and summarize the main modules."
```

After installing the binary:

```bash
moyai run --dir /path/to/workspace "Add tests for the parser."
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

Release builds include `moyai.exe` for CLI/TUI workflows and `moyai-desktop.exe` for launching the desktop app directly. On Windows, double-click `moyai-desktop.exe` to open the desktop app. The desktop runtime window/taskbar icon uses `logo/fabicon/android-chrome-512x512.png`, and the Windows executable resource uses the multi-size `logo/fabicon/moyai_app_icon.ico`.
When no workspace is specified, the desktop app opens the current Windows user's Desktop folder as the default workspace.

## Configuration

moyAI reads configuration from global config, workspace config, environment variables, and CLI overrides.

On first normal app startup, moyAI creates a global config file with editable default values if it does not already exist:

- `%APPDATA%\midi-ai-labs\moyai\config\config.toml`

The release folder does not need to contain a `.toml` file. Place the binary in a stable install directory such as `C:\tools\moyai\`; moyAI stores user configuration and session data under the Windows user profile.

Workspace config files:

- `moyai.toml`
- `.moyai/config.toml`

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

Example:

```toml
[model]
base_url = "http://127.0.0.1:1234"
# LM Studio-hosted qwen3.6-35b-a3b, lmstudio-community build.
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

## Instruction Files

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
cargo run -- preflight run
```

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.

### Third-party Software

moyAI uses third-party software that is governed by its own license terms.

- Slint: https://slint.dev/
  - This application uses Slint as a UI framework.
  - moyAI uses Slint under the Slint Royalty-free License.
  - Slint also offers GNU GPLv3 and paid commercial license options.
