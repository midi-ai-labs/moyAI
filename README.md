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
- Evidence-first task planning with canonical `update_plan` as a client-visible progress projection, not an execution gate.
- Immutable turn/step context, canonical protocol history, and atomic response-scoped assistant/raw-tool-call commits keyed by `ModelResponseId`.
- LM Studio Responses API support with turn-scoped `previous_response_id` continuity and typed reasoning summaries.
- Automatic LLM semantic compaction near the context threshold, using response/call-output semantic units, a prepared-request token target, giant-item map/reduce, replacement lineage, and non-destructive no-progress handling.
- LM Studio metadata discovery through `/v1/models` and `/api/v1/models`.
- Workspace search, directory inspection, guarded file reads, diff-based edits, and shell execution.
- Permission presets: `default`, `auto_review`, and `full_access`. Desktop remembers the selected preset globally for the next launch and also on the currently open root session. In TUI, F8 remembers the choice for the open root session, or globally when no session is open; child agent sessions cannot replace the root access owner. `auto_review` keeps deterministic low-risk operations as a fast path and sends remaining permission requests to a separate, tool-less AI reviewer using the configured model. The typed JSON allow/deny `outcome` is the sole decision owner; omitted risk, user authorization, and rationale metadata receive safe defaults. Only a normally completed `allow` runs automatically, while deny, an unknown outcome, timeout, transport, response-shape, parse, or provider-terminal failures fall back to human confirmation. `full_access` automatically allows detected risks inside the configured boundary and still confirms outside-boundary actions. Choosing **do not run; change instructions** records only the requesting tool as `Declined`, records other tools stopped by the interruption as `Cancelled`, and interrupts the current root task—even when a child agent requested approval—without feeding a denial back to the model. The internal **deny and continue** decision, an external Stop, and an operational failure remain separate typed outcomes. Neither classification nor AI review is an OS filesystem sandbox, and commands run with the current user account.
- Vision-capable model support for image attachments.
- Optional Docling Serve and HTTP MCP integration for document-heavy workflows.
- Local instructions from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md`, and local `SKILL.md` files.
- Canonical protocol session history, typed turn terminals, Markdown export, and lightweight live-smoke artifacts.
- Optional root-scoped multi-agent collaboration with separate child sessions and visible Desktop activity.

## Current Release

The current beta release is available here:

[**moyAI v0.7.0 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0)

The canonical runtime/storage cutover described in this source tree is still being verified on its
feature branch and is not part of the published v0.7.0 package until a later release is completed.

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
provider_api_mode = "auto"
reasoning_summary = "none"
request_timeout_ms = 300000
stream_idle_timeout_ms = 300000
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

`request_timeout_ms` limits how long moyAI waits for provider response headers, while
`stream_idle_timeout_ms` limits a period with no SSE event after streaming starts. Both default to
300,000 ms. They are no-progress deadlines, not a cap on total generation time; explicit config or
environment overrides remain supported.
`max_retries` applies only to connection failures and pre-stream HTTP 429/5xx responses, with every
retry delay capped at 30,000 ms. A response-header timeout or a failure after an SSE response starts
is terminal and is not replayed automatically.
The separate model-availability action uses its own 120,000 ms per-request probe deadline and does
not run as part of normal turn admission.
Desktop cold start validates only the local configuration: it does not load the provider catalog,
run the availability diagnostic, or probe Docling. Provider discovery starts only when the user
chooses model loading, and Docling connects only when an explicitly requested operation uses it.
Configuration parsing is strict at every nested section. Unknown or retired keys, including
`stream_max_retries`, are reported as errors instead of being silently retained as no-op settings.

When MCP is enabled, each callable server tool needs an explicit effect route. Unlisted routes fail
closed; in the internal Plan mode, only routes explicitly classified as `read` are callable.

```toml
[mcp]
enabled = true

[[mcp.servers]]
id = "internal"
enabled = true
transport = "http"
base_url = "http://127.0.0.1:8123/mcp"
timeout_ms = 120000

[[mcp.servers.tool_routes]]
name = "inspect"
effect = "read"

[mcp.servers.headers]
```

Common environment variables:

- `MOYAI_BASE_URL`
- `MOYAI_MODEL`
- `MOYAI_PROVIDER_METADATA_MODE`
- `MOYAI_PROVIDER_API_MODE`
- `MOYAI_CHAT_COMPLETIONS_REASONING_PARAMETERS`
- `MOYAI_REASONING_EFFORT`
- `MOYAI_REASONING_SUMMARY`
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
Provider metadata mode does not select a model-name-specific prompt profile or inject a hidden
language / no-thinking prefix. Tool, image, and parallel capability have one owner in `ModelPolicy`;
provider policy owns only API mode and reasoning transport. Provider mode still selects the wire
encoding for tool choice and the provider-specific availability probes; those probes do not become a
second capability owner. LM Studio mode keeps named tool requests provider-portable by sending
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

`provider_api_mode = "auto"` is the default transport policy. It resolves LM Studio native mode to
`/v1/responses` and OpenAI-compatible-only mode to `/v1/chat/completions`. The Responses transport
keeps provider state within the active run by reusing `previous_response_id` and sending only new
tool outputs or steer input after a completed response. Raw reasoning text is neither replayed nor
stored as assistant context. A requested typed reasoning summary is a runtime-only client event, not
a durable conversation or runtime row.

Reasoning controls are optional. A reasoning-capable model can use, for example,
`reasoning_effort = "medium"` and `reasoning_summary = "concise"`. Responses has a standard typed
contract. Chat Completions varies by provider, so reasoning parameters remain fail-closed unless
`chat_completions_reasoning_parameters = "effort_only"` or `"effort_and_summary"` is configured.

## Runtime and History Continuity

Each turn resolves one immutable `TurnContext` for its turn/admission identity, selected
model/provider policy, optional multi-agent mode, and durable collaboration-mode instruction.
It also captures one turn-start wall-clock snapshot. Step/world-state refreshes reuse that snapshot so
a clock tick alone does not invalidate Responses continuity; an explicit `current_time` tool call still
performs a fresh read.
Session/workspace state remains in `SessionContext`, the root-scoped agent context owns the agent-tree
role, and the live permission owner supplies the current access mode without duplicating those values
into the turn. Each model request captures a `StepContext`
for the current world state, skills, and optional external-tool availability. The same step produces
the advertised tool schema, execution router, and effect classification, so visibility and safety are
not separate execution contracts. MCP effects come only from explicit per-server tool routes; an
unlisted route is rejected.

Canonical protocol history is the conversation source of truth. User and steer turns enter it directly;
assistant messages, raw tool calls/outputs, collaboration-mode instructions, and compaction lineage are
stored as typed items. A canonical tool call preserves the provider's `tool_name` and
`arguments_json` strings; typed-name parsing, JSON parsing, and schema validation are transient
execution steps. Assistant text and every raw tool call from one provider response share a
`ModelResponseId` and commit in one database transaction before any tool executes, so a partial
response cannot remain or be rewritten to `Invalid` / `null` when parsing fails.
Tool result title, metadata, output, and error live only in canonical `ToolOutput`; the tool sidecar
keeps lifecycle, truncation-path, and timestamp data. Committed durable events are published only
after their storage transaction; streaming deltas and reasoning summaries use a separate runtime-only
path and are not persisted as conversation fragments. A typed turn terminal stores the final status,
finish/interruption cause, final response identity, and metrics.
Protocol writes are limited to their atomic session/runtime owners. The generic protocol query/fork
surface cannot append arbitrary event bundles, and the runtime recording sink accepts only its explicit
projection allow-list rather than duplicating model/tool/file/terminal ownership.
TUI does not insert a submitted user/steer row or clear the composer optimistically. It tracks root-run
and steer submission identities, projects the row after durable `UserTurnStored` / successful
`SteerStored` acceptance, and clears only a draft whose revision and text are still unchanged. A
pre-admission/storage failure or a post-submit edit keeps the draft and creates no phantom user row.

Durable run admission commits the run identity and turn identity together, so there is no persisted
state where a run owns the session without an active turn. Session rollback, filtered fork,
expired-run recovery, and active mail-versus-terminal settlement each have one atomic
storage/admission boundary. In particular, mail committed first is drained in the same turn, while a
terminal committed first prevents a later active-recipient append.

The feature-branch V37 upgrade is intentionally destructive for old tool-call turns that lack an exact
provider-response identity: it does not invent lineage, and removes the affected turn's protocol and
sidecar evidence in one transaction while retaining other turns. Back up a database before testing
this source-tree upgrade against existing data.

The default tool surface exposes `update_plan` for non-trivial work. Its structured result is only a
client-visible plan projection: it does not decide the next tool, end the turn, or trigger
compaction. A durable Plan mode exists internally, keeps `update_plan`, and hides mutation tools, but no
CLI, TUI, or Desktop mode selector is currently exposed.

When a prepared request reaches the model policy's context threshold, moyAI selects model-visible
semantic units rather than a fixed item count. One provider response's assistant text, calls, and
settled outputs stay together, and compaction stops before an unsettled call. It selects units until
the prepared request reaches the model token target; a single giant item is split to the model's input
capacity and summarized through map/reduce. The exact replacement lineage is committed while original
history remains stored. If character volume or the prepared-request token estimate does not shrink,
or summarization otherwise fails, history remains unchanged; below the hard limit the original history
continues, and at the hard limit the run fails explicitly.

An active session goal is not declared successful after an arbitrary number of idle continuations. It
continues until the goal state, its token/elapsed budget, cancellation, or a typed terminal provides a
semantic stopping condition.

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
  or `"none"`; `"all"` copies the currently active user turns, visible assistant messages, durable
  collaboration-mode instruction, and active compaction summary. History replaced by that summary is
  not resurrected, and reasoning, tool traffic, retired control state, and permission evidence are not
  copied. Sub Agent activity is recorded only while its owning root session has a fresh active turn.
- Desktop shows active work as clickable inline agent chips and collapses terminal activity into one
  history summary. Activating that summary, or the compact summary in Output, opens a read-only
  Sub Agent pane for the current root task with status groups, task, current work, result, and child
  session ID. It does not navigate to the child session and becomes a right-side drawer in narrow
  windows. Permission prompts identify the requesting agent and are serialized. While any agent in
  the current tree is active, new-chat, session, project, and workspace navigation is blocked. This
  keeps the current root task selected and preserves permission and Stop routing; Stop cancels the
  whole tree.
- Rust supplies typed session status, transcript-row kind, and cancel availability to Desktop. The
  frontend does not infer them from labels, and a turn without a durable terminal is shown as
  incomplete rather than completed.

## Startup Checks

On cold start, `moyai-desktop.exe` shows the moyAI splash for at least five seconds and validates
local values only:

- global config file state
- workspace availability
- configured provider base URL and model value
- configured Docling enabled flag and base URL

The splash does not wait for network activity. Cold start sends no provider catalog, availability,
or Docling health request. Invalid local settings open Settings or LLM URL; live connectivity is
checked only by the explicit model-load/diagnostic action or when the configured service is used.

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
