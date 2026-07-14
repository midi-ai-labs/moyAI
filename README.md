<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>A local-first coding agent for private workspaces, local LLMs, and closed-network development.</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.7.0-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.ja.md">日本語 README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0">Download release</a>
  ·
  <a href="#quick-start">Quick Start</a>
  ·
  <a href="#configuration">Configuration</a>
</p>

<p align="center">
  <img src="logo/moyai-screenshot-sample.png" alt="moyAI Desktop screenshot" width="920">
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
| No implicit bootstrap | moyAI does not automatically install dependencies, download runtimes, set up package managers, or fetch external repositories. A user-requested shell command can still access the network when the active permission policy allows or confirms it. |

## Highlights

- Tauri Desktop app with project chat, quick chat, transcript, artifacts, settings, and provider discovery.
- One Desktop instance per user; launching it again restores the existing window.
- CLI and TUI for terminal-centered workflows.
- OpenAI-compatible local LLM connection with model availability checks.
- LM Studio metadata discovery through `/v1/models` and `/api/v1/models`.
- Workspace search, directory inspection, guarded file reads, diff-based edits, and shell execution.
- Permission presets: `default`, `auto_review`, and `full_access`. Desktop remembers the selected preset globally for the next launch and also on the currently open root session. In TUI, F8 remembers the choice for the open root session, or globally when no session is open; child agent sessions cannot replace the root access owner. `auto_review` keeps deterministic low-risk operations as a fast path and sends remaining permission requests to a separate, tool-less AI reviewer using the configured model. The typed JSON allow/deny `outcome` is the sole decision owner; omitted risk, user authorization, and rationale metadata receive safe defaults. Only a normally completed `allow` runs automatically, while deny, an unknown outcome, timeout, transport, response-shape, parse, or provider-terminal failures fall back to human confirmation. `full_access` automatically allows detected risks inside the configured boundary and still confirms outside-boundary actions. Choosing **do not run; change instructions** records only the requesting tool as `Declined`, records other tools stopped by the interruption as `Cancelled`, and interrupts the current root task—even when a child agent requested approval—without feeding a denial back to the model. The internal **deny and continue** decision, an external Stop, and an operational failure remain separate typed outcomes. Neither classification nor AI review is an OS filesystem sandbox, and commands run with the current user account.
- Vision-capable model support for image attachments.
- Optional Docling Serve and HTTP MCP integration for document-heavy workflows.
- Local instructions from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md`, and local `SKILL.md` files.
- Protocol-first session history with Markdown export and lightweight live-smoke artifacts.
- Optional root-scoped multi-agent collaboration with separate child sessions and visible Desktop activity.

## Current Release

The current beta release is available here:

[**moyAI v0.7.0 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0)

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
npm ci
npm run build:desktop-web
cargo build --release --bin moyai --bin moyai-desktop --bin moyai-cleanup
```

Windows release package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 0.7.0 -ManualGuiStResultsPath path\to\RESULTS.md
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
provider_metadata_mode = "lm_studio_native_required"
context_window = 131072
supports_tools = true
supports_images = true
max_output_tokens = 8192

[model.extra_body_json]
num_ctx = 131072

[permissions]
access_mode = "auto_review"

[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"

[mcp]
enabled = false
```

Common environment variables:

- `MOYAI_BASE_URL`
- `MOYAI_MODEL`
- `MOYAI_PROVIDER_METADATA_MODE`
- `MOYAI_CONFIG_PATH`
- `MOYAI_DATA_DIR`
- `MOYAI_ACCESS_MODE`
- `MOYAI_REQUEST_TIMEOUT_MS`
- `MOYAI_STREAM_IDLE_TIMEOUT_MS`
- `MOYAI_CONTEXT_WINDOW`
- `MOYAI_MAX_OUTPUT_TOKENS`
- `MOYAI_SUPPORTS_IMAGES`
- `MOYAI_MULTI_AGENT_ENABLED`
- `MOYAI_MULTI_AGENT_MODE`
- `MOYAI_MULTI_AGENT_MAX_AGENTS`
- `MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS`
- `MOYAI_DOCLING_ENABLED`
- `MOYAI_MCP_ENABLED`

Use `provider_metadata_mode = "openai_compatible_only"` or
`MOYAI_PROVIDER_METADATA_MODE=openai_compatible_only` for OpenAI-compatible servers that do not
provide LM Studio's native `/api/v1/models` metadata endpoint, such as vLLM/vLLM-MLX.
In this mode, every OpenAI-compatible chat request prefixes the configured system prompt with the
language / no-thinking policy required for qwen3.6 hosted behind vLLM-compatible servers.
The same mode is also the provider profile boundary for tool-choice serialization and model
availability gates. LM Studio mode keeps named tool requests provider-portable by sending
`tool_choice = "required"` and gates on `required` / strong `auto` tool-call probes; OpenAI-compatible
mode sends OpenAI named function `tool_choice` objects and requires the named probe to pass.
A saved LM Studio lab-profile example lives under `docs/testing/provider-profiles/`. It is not a
product default: copy it to an isolated config, update the endpoint/model for the current environment,
and select it with `MOYAI_CONFIG_PATH` without overwriting the user-wide config.
The Tauri Desktop `LLM URL` overlay exposes the same mode switch beside the provider URL and model list.
It also owns `context_window` and `max_output_tokens` inputs so vLLM/vLLM-MLX limits can be managed
inside moyAI instead of relying on shell environment variables. Current vLLM-MLX `/health` and
`/v1/status` responses expose the hosted model name, but not the server startup `--max-tokens` /
`--max-request-tokens` values, so moyAI auto-detects the model and keeps request limits as managed
config unless a provider exposes those fields in `/v1/models`.

## Multi-Agent Collaboration (Opt-in)

Multi-agent collaboration is disabled by default. Set `[multi_agent].enabled = true` in Settings or
the config file to expose these six tools to the model: `spawn_agent`, `send_message`,
`followup_task`, `wait_agent`, `interrupt_agent`, and `list_agents`.

- `mode = "explicit_request_only"` delegates only when the user explicitly requests agents,
  sub-agents, delegation, or parallel agent work. `mode = "proactive"` also lets the model delegate
  bounded independent work when doing so materially improves quality or latency.
- The first release fixes tree depth at one level: only the root may call `spawn_agent`, and a child
  cannot spawn another Sub Agent.
- `max_concurrent_agents` is the root-inclusive limit for simultaneously active agents. The default
  `4` therefore allows the root plus up to three children to run at once. Completed agents remain
  listed and available for follow-up work but no longer consume an active slot, so the parent may
  spawn further bounded tasks sequentially.
- `max_concurrent_model_requests = 1` keeps local-LLM model requests within the tree serialized by
  default, while agents can still make progress independently around tool and review work. Raise it
  only when the configured inference server can safely sustain parallel requests.
- Each child is a separate durable session linked to its parent. Normal project/session lists keep
  those implementation sessions hidden. `spawn_agent` accepts `fork_turns = "all"` (the default)
  or `"none"`; `"all"` copies only user turns and visible assistant messages, not reasoning, tool
  traffic, internal control items, or permission evidence.
- Desktop shows active work as clickable inline agent chips and collapses terminal activity into one
  history summary. Activating that summary, or the compact summary in Output, opens a read-only
  Sub Agent pane for the current root task with status groups, task, current work, result, and child
  session ID. It does not navigate to the child session and becomes a right-side drawer in narrow
  windows. Permission prompts identify the requesting agent and are serialized. While any agent in
  the current tree is active, new-chat, session, project, and workspace navigation is blocked. This
  keeps the current root task selected and preserves permission and Stop routing; Stop cancels the
  whole tree.

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
cargo fmt --all -- --check
cargo check --all-features
cargo test -- --test-threads=1
npm run test:desktop-web
npm run build:desktop-web
```

Desktop interaction changes also require operating the actual Tauri window and saving screenshot evidence under `../project_sandbox/<task>/`; a build and startup check alone do not prove UI behavior.

Published release packages must also pass a visible Desktop GUI manual ST before upload.
Record the result in a UTF-8 Markdown artifact containing `Manual ST Gate: PASS`, then pass that
file through `scripts/package-release.ps1 -ManualGuiStResultsPath ...`; the artifact is copied into
the release zip under `docs/release/manual-gui-st-results.md`.

## Status

moyAI is currently developed and tested primarily on Windows. The main development profile uses `qwen/qwen3.6-35b-a3b` hosted by LM Studio, especially the `lmstudio-community` build.

Other OpenAI-compatible models can be used, but model behavior, tool-use quality, context length, and vision support vary by provider and model.

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
