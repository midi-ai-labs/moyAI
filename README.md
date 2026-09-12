<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>A local-first coding agent for private workspaces, local LLMs, and closed-network development.</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v2.1.1"><img alt="Release" src="https://img.shields.io/badge/release-v2.1.1-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.ja.md">日本語 README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v2.1.1">Download release</a>
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

This development branch targets **Desktop v3.0.0 / LYNX**. The **moyAI Hub** panel
now includes Hub-managed device enrollment, receiving tasks, and choosing other devices.
Import the shared Hub configuration to request enrollment automatically. After the Hub administrator
approves the device, it connects without an enrollment code; enable receiving or select an allowed peer.
A new worker can start with Hub configuration before setting up
a Direct model. The shared file contains the Hub URL and public CA trust; each device generates
its own private key. On enrolled Desktop devices, receiving IP, port, TLS credentials and short-lived
authorization are managed by the apps.
Earlier verification on one Windows PC covered the actual Hub/Desktop GUI and separate sender/receiver
runtimes for shared-config setup, enrollment, temp receiving, directed permission, peer selection,
named approval and CPU task results returned to the sender. The current MCP history screens and Markdown
export have also been checked in the actual Windows GUI. Physical multi-PC acceptance and verification of
the additional approval, diagnostics and artifact operations remain outstanding; see the build's release notes.

Hub administrators assign groups and directed permissions. Joining a Hub does not connect every device
to every other device. Receivers choose a project or **temp**, execution permissions and model routing.
The receiving agent performs the task locally and retains its canonical history; a temp CPU query
uses the agent's permitted tools on that device. Authorized redelegation retains the original caller
and task constraints. Receiving OFF blocks new work; existing jobs have separate status and stop controls.
Startup and tray receiving are explicit preferences. See the [device-network guide](docs/hub-device-network-guide.md)
and [design and acceptance status](docs/design/hub-device-network.md).

Development servers can use `shell_start`, `shell_status`, and `shell_stop` for finite,
app-owned command lifetimes. Starting a process does not attest to HTTP readiness.
See the [managed command guide](docs/managed-shell-guide.md) for time limits and ownership.

The model tab retains independent Main / Side Chat model selection and review. The Hub route uses
a companion Chat Completions / Responses gateway that counts its own forwarded HTTP requests,
with no implicit Direct fallback. Per-context catalog review includes saved comparison baselines;
Hub-only Side Chat, prompt enhancement, and independent child-agent execution are implemented
and undergoing final verification.
Direct settings remain available. **MCP履歴** provides **MCP指示** and **MCP実行** views of locally saved
delegation records, results and errors, with Markdown export. A compatible Hub can also retrieve and save
device history snapshots; update both Hub and Desktop. See the [MCP history guide](docs/mcp-history-guide.md).
The manual publishing editor and its settings/start commands have been retired. Existing profile files,
credentials, certificates and execution history are retained; updating or restarting Desktop does not reopen
those listeners. Configure reception explicitly through **moyAI Hub → 端末連携**. Read-only grants and shared
tokens are never upgraded automatically to agent authority. Outbound MCP connections and the shared
transport remain available. See [MCP compatibility](docs/design/mcp-publish-foundation.md).
The implementation includes delegation recovery after a Hub restart, certificate renewal, approval on
the receiving device, explicit versioned inputs and bounded UTF-8 artifact export to a new folder on Windows.
Physical multi-Windows and three-device redelegation acceptance remain outstanding. Automatic application
of artifacts to the original project, OS service operation and automatic rollout of a replacement CA remain unsupported.
The published v2.1.1 download below remains the released version.

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

- Tauri Desktop app with project chat, quick chat, transcript, artifacts, settings, provider discovery, and a tool-less session-scoped Side Chat that can use a model separate from the main task. Immediately before each Side Chat POST, moyAI captures the exact owning session's active canonical history and append fence; an optional quote is bound to one stable transcript/artifact row and must still match that snapshot. Storage work is bounded to 65,536 append-only source items, 16,384 derived active items, and 8,192 eligible semantic units, failing before provider transport when any bound is exceeded. Untrusted quote and canonical evidence text is XML-entity encoded before it enters the owner-context envelope, so evidence cannot forge its structural delimiters. The quote is added to the Side draft without auto-send, and Side Chat never reads live workspace state or changes the main composer.
- Desktop renders canonical history as a continuous conversation: user bubbles and plain assistant responses have no display-only step numbers, completed work history is collapsible without swallowing the root Agent's final response, and older bounded chunks prepend in place with a left-side hover/jump rail instead of replacing the page.
- One Desktop instance per user; launching it again restores the existing window.
- Desktop Stop validates the projected workspace, root session, run generation, and Agent Tree epoch, so stale UI actions cannot cancel a later run. Settings values, baseline, dirty state, and monotonic revision exist only in one frontend-local draft owner. Rust projects typed clean/dirty capability variants and statelessly validates a complete draft plus a decimal-string config-generation target before Apply, Save, Reset, or another config-owner mutation. Commit builds one complete temporary `ResolvedConfig`, preserving cleared optional values instead of re-layering them. Active-turn steer clears input only after durable acceptance.
- CLI and TUI for terminal-centered workflows.
- OpenAI-compatible local LLM connection with explicit model availability diagnostics. moyAI connects to the configured external HTTP endpoint; it does not launch or supervise the provider process.
- Evidence-first task planning with canonical `update_plan` as a client-visible progress projection rather than an execution or tool-access gate. In proactive mode, static model instructions require minimum grounding followed by an early plan before broad investigation.
- One immutable `ResolvedTurnConfig`/turn/step context captured at admission, canonical protocol history, and atomic response-scoped assistant/raw-tool-call commits keyed by `ModelResponseId`.
- LM Studio Responses API support with full canonical HTTP input replay and typed reasoning summaries.
- Automatic LLM semantic compaction near the context threshold, using provider-reported total usage plus a Codex-style UTF-8-bytes/4 local suffix estimate, a full-request local fallback, oldest semantic-unit prefix summary requests with one aligned saturation/overflow retry, and durable replacement lineage for only the summarized units.
- LM Studio metadata discovery through `/v1/models` and `/api/v1/models`.
- Bounded workspace traversal/search/directory inspection with model-visible continuation cursors, guarded line-aware file-read pages with exact next offsets and no read spool path, diff-based edits, and shell execution.
- A selected nested directory remains the tool and sandbox authority boundary even when an ancestor is the Git project root; reopening its session restores that exact directory.
- File writes and patches use one stable-handle, no-clobber conditional commit for create, update, delete, and rollback. A concurrent external replacement wins without being overwritten; if restoration cannot reclaim the target name, moyAI reports the preserved backup path. Parent directories are not created implicitly, so create the parent first.
- On Unix, moyAI cannot prove that a writable descriptor opened before an update or delete no longer references the detached inode. Creation remains unchanged, but an existing-file update installs the new target and a delete detaches the target while retaining the old inode at a private backup path; both report a typed partial-commit error instead of claiming safe cleanup. Inspect and reconcile the reported backup because a pre-opened writer can still modify it.
- Permission modes: **Ask for approval** (`default` / 承認を求める), **Approve for me** (`auto_review` / 代理で承認), and **Full access** (`full_access` / フルアクセス). Ask and Auto share one deterministic admission policy and the same Windows `workspace-write` restricted-token/ACL profile; explicit `sandbox_permissions: "require_escalated"` plus `justification`, or a detected destructive/network/external/authority effect, goes to a human in Ask. In Auto, the turn-captured canonical API mode selects an exact tool-less Responses or Chat Completions AI Guardian request for every current connection profile; a future unknown wire fails closed before Guardian contact without human/Ask fallback. Concrete oMLX compatibility is qualified separately by actual UAT. The Windows backend identity-pins admitted roots, shell/formatter executables, and selected existing authority carveouts, content-pins protected regular files, gives each launched process/thread an explicit system-only descriptor, inherits only stdio, applies Job process-tree/UI restrictions before resume, and fails closed without an unrestricted retry. This unelevated profile is a finite existing-object defense, not a complete Windows namespace or Codex-enforcement equivalent: absent authority names, unrelated nested instruction files, protected descendants with overriding explicit/inheritance-disabled DACLs, pre-confirmation uninspected outside paths, direct sockets, same-user host-process memory, and same-desktop synthetic input remain residuals. Its ACL preflight can propagate through existing trees synchronously and is not covered by the child timeout. Full Access and an approved process elevation run `Unrestricted` as the current user, so their child filesystem mutations do not pass through typed file guards; typed `write`/`apply_patch`, MCP/Docling, and process lifecycle checks keep their own guards. A committed mode change affects the next permission decision, while a pending request and an admitted effect retain their original decision/profile. Native process sandboxing is currently Windows-only; workspace-mode process effects fail closed elsewhere. A future elevated dedicated-identity/firewall/private-desktop backend is required for the hard boundary.
- Vision-capable model support for image attachments.
- Optional Docling Serve and HTTP MCP integration for document-heavy workflows.
- Local instructions from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md`, and local `SKILL.md` files.
- Canonical protocol session history, typed turn terminals, Markdown export, and lightweight live-smoke artifacts.
- Short typed provider/status labels, an attention-first progress summary bounded to eight items, complete-detail routes through canonical history/export, terminal-derived session usage that distinguishes missing/partial/complete measurement, and durable feedback that remains identical after reopen and export.
- Recursive multi-agent collaboration, available by default for explicit delegation requests, with the normal and collaboration tools available to every agent, separate descendant sessions, and visible Desktop activity.

## Current Release

The current release is available here:

[**moyAI v2.1.1 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v2.1.1)

v2.1.1 is the final release in the v2 line. It adds independently configurable Main and Side Chat
system prompts, reorganizes Settings around global, session-scoped, and Desktop-owned state, and
strengthens owner-bound Side Chat context, quoting, navigation, and restart continuity. The release
also extends exact tool-less AutoReview Guardian transport to every current provider profile,
including OpenAI-compatible endpoints such as oMLX, and adds fail-closed hardening around executable
identity, MCP origins, provider diagnostics, secrets, and Windows sandbox admission.

The Windows release zip includes:

- `bin/moyai.exe` for CLI / TUI workflows
- `bin/moyai-desktop.exe` for the Desktop app
- `bin/moyai-cleanup.exe` for resetting user-wide moyAI AppData to first-run state
- bundled `ui/desktop-web/dist/` assets
- README files, license, release notes, config example, getting-started guide, and in-package SHA256 checksums

The GitHub Release publishes the zip together with its external manifest and zip SHA256 sidecar.

On the target Windows machine, you do not need npm, the Rust toolchain, internet access, or a local web dev server.

## Quick Start

1. Start, or connect to, an OpenAI-compatible LLM server reachable at the URL you plan to configure.
2. Download and extract the latest release zip.
3. Launch `bin/moyai-desktop.exe`.
4. On first launch, complete the fullscreen Initial Setup flow. Enter or import the provider, model, permission, and optional-tool settings, review the local validation result, then choose **Finish and open moyAI**. Model loading and provider/Docling diagnostics run only when you explicitly request them; an unavailable endpoint is reported as a warning and does not block a locally valid setup.
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
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 2.1.1 -ManualGuiStResultsPath path\to\RESULTS.md
```

Run packaging from the clean source commit for that release. If `v<version>` already exists, the
script permits a publishable rebuild only from the commit identified by that tag; use a newly
synchronized version for later source.

By default, release artifacts are written outside the repository under `project_sandbox/releases/`.

## Configuration

moyAI uses one user-wide config file, then layers environment variables, a durable root-session override, and CLI/run overrides where applicable. The left-rail **Connection Settings / 接続設定** shortcut opens **Settings**. Its navigation separates **Global Settings** (including **Main Chat Settings** and **Side Chat Settings**), **Session-scoped Settings** (the **Session Overrides** entry), and **Desktop Preferences**. Global Main and Side Chat settings are both part of the importable user-wide TOML configuration, while the Session Overrides entry opens the smaller **Session Settings** panel for the current root Main-chat session only. Session Settings never exposes a global-save action and does not edit Side Chat defaults.

Default Windows config path:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

The release folder and workspace folders do not need their own config file. Desktop, TUI, and CLI share the user-wide baseline; a Desktop root session may additionally retain its own complete provider connection (connection type, URL, model, optional API-key environment-variable name, and custom headers), moyAI-local context budget, and access-mode values when reopened. Provider/model/context changes affect turns admitted after Apply. A committed access-mode change affects the next permission decision, including one made later by an already-running root or child, while an existing pending decision and an already-admitted effect keep their original policy.

Initial Setup TOML import is read-only until Finish: the selected file is strictly parsed into the local wizard draft without materializing environment overrides, and neither the source nor the current global configuration is mutated by choosing it. Only a successful Finish persists the validated draft and clears the first-run setup requirement. Less common typed fields remain editable in the wizard's collapsed Advanced area, which opens when one of those fields needs correction.

In Session Settings, leaving **moyAI local context budget** blank removes that root-session override and inherits the global value. This budget controls local input accounting and compaction only; it is not sent to the provider as a context-window or model-load setting.

Sensitive JSON settings—custom provider headers/body, Docling headers, and MCP server definitions—are projected publicly as **configured, hidden** rather than returning their raw values. Leaving a complete sensitive-field draft blank preserves the existing value. To replace or clear it, submit explicit valid JSON such as `{}` or `[]`. API-key environment-variable names may be displayed and persisted, but the resolved environment value is never persisted or projected.

Example:

```toml
[model]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-27b"
provider_profile = "lm_studio"
# system_prompt = """Additional instructions for Main.""" # optional
# api_key_env = "OPENAI_API_KEY" # optional; names an environment variable
request_timeout_ms = 3600000
context_window = 131072
supports_tools = true
supports_images = true

[side_chat]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-27b"
provider_profile = "lm_studio"
# system_prompt = """Additional instructions for Side Chat.""" # optional
request_timeout_ms = 3600000
connect_timeout_ms = 10000
max_retries = 2
context_window = 131072

[permissions]
access_mode = "default"

[multi_agent]
enabled = true
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"

[mcp]
enabled = false
```

`model.system_prompt` optionally adds instructions for Main. moyAI keeps its built-in system and
default-profile instructions first, then appends the configured text under a
`## User-configured system prompt` section. Leading and trailing whitespace is removed; an omitted or
blank value adds nothing. The limit is 16,384 Unicode characters, and changes affect turns admitted
after the updated configuration is applied.

`[side_chat]` is an independent global configuration section; it does not inherit values from
`[model]`, even when their defaults happen to match. It can be imported and saved with the rest of the
user-wide configuration. `side_chat.system_prompt` is appended after the built-in Side Chat prompt and
is counted during context preflight. The blank and 16,384-character rules are the same as for Main.

The first time a Side Chat is created for a session—or when it is created again after an explicit
close—moyAI snapshots the current global Side Chat provider, model, prompt, timeout, retry, and context
settings into that conversation. An existing Side Chat keeps its captured settings, history, and draft
across pane hiding, session navigation, window closing, and app restart. Explicitly closing it deletes
that Side Chat conversation, draft, and snapshot, but never deletes or resets the global `[side_chat]`
settings.

`request_timeout_ms` is moyAI's client-side liveness timeout. From the first POST attempt through a
successful response header it is one absolute deadline covering eligible retry delays, request upload,
and header wait. After a successful header, the same value becomes the maximum interval between decoded
SSE events and is renewed whenever stream progress arrives; a progressing generation therefore has no
total-duration cutoff. The value is never sent to the host. It defaults to 3,600,000 ms (60 minutes),
which is also the maximum accepted value. Desktop Settings, TUI, imported TOML, and
`MOYAI_REQUEST_TIMEOUT_MS` all use this same owner. The legacy `stream_idle_timeout_ms` TOML key and
`MOYAI_STREAM_IDLE_TIMEOUT_MS` environment variable remain accepted for migration only: a legacy-only
value becomes the request timeout, equal old/new values are accepted, and conflicting values are rejected
with a config error instead of silently choosing one.
Maximum output length is owned by the hosting provider. moyAI omits both Responses
`max_output_tokens` and Chat Completions `max_tokens`, so ordinary text, reasoning, and serialized
tool-call arguments use the limit configured in LM Studio, oMLX, or the selected host. A provider-side
`response.failed` is mapped to a stable typed public failure and is not treated as a locally parsed or
executed tool call. Its raw provider code/message remains private diagnostic evidence and is not written
to the durable terminal, canonical history, or Markdown export.
`max_retries` applies only to retryable connection/transport failures before any HTTP response, with
every retry delay capped at 30,000 ms. A response-start timeout, any HTTP error response (including
429/5xx), or a failure after an SSE response starts is terminal and is not replayed automatically.
The separate model-availability action uses its own 120,000 ms per-request probe deadline and does
not run as part of normal turn admission.
Desktop cold start validates only the local configuration: it does not load the provider catalog,
run the availability diagnostic, or probe Docling. Provider discovery starts only when the user
chooses model loading, and Docling connects only when an explicitly requested operation uses it.
Configuration parsing is strict at every nested section. Unknown or retired keys, including
`stream_max_retries`, are reported as errors instead of being silently retained as no-op settings.
The error names the exact config file that failed. Existing user-wide files are not silently rewritten:
remove or replace retired `stream_max_retries`, `[model_providers.*]`, and
`session.auto_compact_*` entries in the reported file before restarting.
Desktop Global Settings keeps its in-progress complete config values, baseline, dirty state, and monotonic
revision in one frontend-local draft owner; Rust keeps no field-value, dirty, or revision mirror.
Global Settings Apply, Save, and Reset send the complete stable key/value draft with the config target;
remembered Access, Provider Apply/Save, and Import use that same global-config draft with their own
target. Rust validates completeness, the current global/effective baseline, and admission before any
side effect. Config generation crosses the Rust/TypeScript boundary as an exact `u64` decimal string,
never a JavaScript number. Global Settings Apply builds one complete temporary `ResolvedConfig`, while
Global Save merges only dirty fields into the current TOML document.

Session Settings has a separate frontend draft limited to the complete provider connection, access mode,
and moyAI-local context budget. Apply sends those values together with the exact workspace, root-session
ID, durable settings revision, config generation, and runtime owner token. Rust derives the canonical
patch and performs a root-only revision CAS; a blank local budget removes that root override and inherits
the global value. Only a correlated success matching the latest local revision and target clears either
draft, and a stale async response cannot mutate or clear a different draft.

When MCP is enabled, each callable server tool needs an explicit effect route. Unlisted routes fail
closed; in the internal Plan mode, only routes explicitly classified as `read` are callable. An HTTP
server uses one canonical absolute `http`/`https` origin with no userinfo or fragment. Discovery and
calls must stay on that origin, redirects are rejected, endpoint/body/header/envelope/response sizes are
bounded, and one absolute timeout covers discovery plus the exact-once effectful call. An effectful
`tools/call` is never retried or redirected to a fallback endpoint.

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
- `MOYAI_PROVIDER_PROFILE`
- `MOYAI_API_KEY_ENV`
- `MOYAI_CONFIG_PATH`
- `MOYAI_DATA_DIR`
- `MOYAI_ACCESS_MODE`
- `MOYAI_REQUEST_TIMEOUT_MS`
- `MOYAI_CONTEXT_WINDOW`
- `MOYAI_SUPPORTS_IMAGES`
- `MOYAI_MULTI_AGENT_ENABLED`
- `MOYAI_MULTI_AGENT_MODE`
- `MOYAI_MULTI_AGENT_MAX_AGENTS`
- `MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS`
- `MOYAI_DOCLING_ENABLED`
- `MOYAI_MCP_ENABLED`

`provider_profile` is one atomic connection contract; catalog discovery and generation transport are
not independently configurable. Use `openai_compatible` for oMLX, vLLM, NVIDIA NIM, and similar
servers that expose `/v1/models` and `/v1/chat/completions`. The available values are:

| Value | Catalog | Generation |
| --- | --- | --- |
| `lm_studio` (default) | LM Studio native metadata | `/v1/responses` |
| `openai_compatible` | `/v1/models` | `/v1/chat/completions` |
| `openai_responses` | `/v1/models` | `/v1/responses` |
| `lm_studio_chat_completions` | LM Studio native metadata | `/v1/chat/completions` |

`api_key_env` is optional and stores the name of an environment variable, not a secret. The variable
is resolved for each request, so rotating its value does not require rewriting the config. Existing
`provider_metadata_mode` / `provider_api_mode` TOML fields and their environment variables remain
accepted only as compatibility input and are normalized to one profile; conflicting old and new values
are rejected. New saves write only `provider_profile`. moyAI does not try another generation endpoint
after a failure because doing so could duplicate a generation or tool call.

The same connection can be supplied as one CLI override layer for a run, an availability check, or
durable session settings:

```text
moyai run --base-url http://omlx-host:8119/v1 --provider-profile openai_compatible "Summarize this project"
moyai model availability --base-url http://omlx-host:8119/v1 --provider-profile openai_compatible
moyai session settings <SESSION_ID> --base-url http://omlx-host:8119/v1 --provider-profile openai_compatible
```

For an authenticated server, set (for example) `OMLX_API_KEY` in the environment of the process that
launches moyAI, then add `--api-key-env OMLX_API_KEY` to the same command. Naming an unset or empty
variable fails closed.

`--api-key-env` names an environment variable; it never accepts the secret itself. Run and
availability overrides apply URL, profile, and API-key environment name as one precedence patch. A
URL or profile change deliberately drops any credential reference not named by that same patch and
always drops inherited custom headers and custom request body instead of forwarding them to a new
connection. For durable
`session settings`, `--provider-profile` therefore requires `--base-url`, and `--api-key-env`
requires both. A `--base-url`-only change to a different endpoint keeps the current profile but
stores that endpoint without the prior API-key reference or custom headers; specifying the same
endpoint is a same-target edit and preserves them. The CLI does not accept custom headers or a custom
body, so a complete CLI session connection stores no custom headers. `model availability
--openai-compatible-only` remains a legacy compatibility flag and cannot be combined with
`--provider-profile`.

The provider profile does not select a model-name-specific prompt profile or inject a hidden language /
no-thinking prefix. Tool, image, and parallel capability have one owner in `ModelPolicy`. Availability
is an explicit catalog/metadata diagnostic; it does not run tool/vision generations or mutate product
capability config.
The current provider contract does not claim server-side strict tool-schema validation. Core and MCP
tool-schema Rust types and both Chat Completions and Responses wire formats have no `strict` field, while raw
arguments are still committed canonically and validated locally against the advertised schema, exact
router name, effect class, and permission boundary before dispatch. In particular, an LM Studio warning
that `strict=true` was ignored does not mean the model failed to load and does not explain a single
long-running generation.
moyAI treats the configured URL as an external HTTP service and never launches, stops, or supervises
the LM Studio process.
Provider reachability, catalog registration, and model-instance load state are separate facts. LM
Studio native metadata maps a non-empty `loaded_instances` array to `loaded`, an explicit empty array
to `not loaded`, and an absent load field to `unknown`; OpenAI-compatible catalog metadata remains
`unknown`. moyAI does not infer on-demand loading from catalog registration.
A saved LM Studio lab-profile example lives under `docs/testing/provider-profiles/`. It is not a
product default: copy it to an isolated config, update the endpoint/model for the current environment,
and select it with `MOYAI_CONFIG_PATH` without overwriting the user-wide config.
The Tauri Desktop provider surfaces expose one **Connection type** selector, the base URL, optional
API-key environment-variable name, and model. They do not expose a second Responses/Chat switch.
It also exposes `context_window` solely as moyAI's local input-accounting and compaction budget. It is
not sent as a provider context-window or model-load setting. Output length and every generation
parameter remain host-owned; provider metadata may report those facts for diagnostics, but moyAI does
not turn them into client-side request overrides.

The `lm_studio` and `openai_responses` profiles use the Responses transport; the
`openai_compatible` and `lm_studio_chat_completions` profiles use Chat Completions. The HTTP Responses transport sends the complete current
canonical input on every request, including any compaction checkpoint, and does not send
`previous_response_id`. Raw reasoning text is neither replayed nor stored as assistant context. A
provider-emitted reasoning summary is a runtime-only client event, not a durable conversation or
runtime row.

Private runtime diagnostics retain a provider request ID and the phases `attempt_started`,
`request_in_flight`, `headers_received`, `first_progress`, `last_progress`, and `provider_terminal`,
plus attempt/elapsed data, a sanitized endpoint, raw provider failure details, and provider-reported
token usage when supplied. Public UI status is instead a short typed phase/failure message with no
request ID, endpoint, elapsed time, or raw provider code/message. Prepared-request diagnostics keep the
logical model-message count separate from the exact HTTP wire input-item count and serialized body
size, without retaining the body. These are transport
boundaries observed by moyAI; they do not infer provider-process startup, server-side acceptance, or
model-instance loading. A long `request_in_flight` phase establishes only that the operation has not
reached response headers. Before POST, moyAI bounds messages, tools, schemas, images, and the exact
serialized wire bytes. After headers, fixed limits bound raw stream bytes, decoded events, tool-call
count, and argument bytes. `request_timeout_ms` separately becomes a rolling inactivity interval between
decoded SSE events; each event renews it, so a progressing stream has no client-side total-duration cutoff.
For an explicit task-local audit, set `MOYAI_HTTP_REQUEST_CAPTURE_DIR` to an absolute directory.
The HTTP transport then writes the exact prepared outbound request JSON plus
API-mode/endpoint/byte-count, capture-stage, and provider-request-ID metadata. The shared request ID
joins this prepared DTO to runtime attempt and terminal phases; the file alone does not prove that a
network attempt started or that the provider received it. Normal sessions retain only redacted
diagnostics. On Unix, the capture directory and files are forced to owner-only `0700` / `0600`
permissions. On Windows, the directory and files inherit Windows ACLs, so choose a location whose
ACL grants access only to the intended account. When capture is explicitly enabled, a capture-write
failure fails request preparation instead of silently losing the evidence.

Sampling, thinking, and output length are owned by the hosting provider. moyAI does not send
temperature, top-p, top-k, penalties, seed, stop sequences, reasoning effort/summary,
`max_output_tokens` / `max_tokens`, or provider-specific extra request body values. Older
TOML and session values for those fields remain readable only as discarded compatibility input;
legacy environment variables are ignored. None of them affect runtime policy, admission,
diagnostics, persistence updates, or either provider wire. Configure such behavior in LM Studio, oMLX, or the
selected hosting service. `context_window` remains a moyAI-local input-accounting capacity and is not
used to load or reconfigure a provider model.
Canonical System and Developer sections remain distinct in the logical model context. At the
OpenAI-compatible wire boundary, moyAI folds them in order into top-level `instructions` for
Responses or one leading `system` message for Chat Completions; it never emits a `developer` role.

## Runtime and History Continuity

Each turn captures one complete `ResolvedTurnConfig` for model, provider target, operation deadlines,
the admitted permission preset, and remaining effective settings, then gives its single `TurnContext`
owner the turn/admission identity, selected policy, and durable collaboration-mode instruction. Partial
configuration is resolved only before admission and is not merged again by later runtime stages.
It also captures one turn-start wall-clock snapshot. Step/world-state refreshes reuse that snapshot so
a clock tick alone does not change model-visible time; an explicit `current_time` tool call still
performs a fresh read.
Session/workspace state remains in `SessionContext`, while the root-scoped agent context owns the
agent-tree role. Model, provider, deadline, multi-agent, and `RunConfigSnapshot` state remain immutable
through the turn. Permission decisions are the narrow exception: immediately before each decision,
moyAI reads the durable root-session access mode, including for child-agent requests. A committed
root-only mode switch therefore applies to the next permission request even in the active turn. It does
not rewrite an already displayed pending request or an already admitted effect. Each model request captures a `StepContext`
for the current world state, skills, and optional external-tool availability. The same step produces
the advertised tool schema, execution router, and effect classification, so visibility and safety are
not separate execution contracts. MCP effects come only from explicit per-server tool routes; an
unlisted route is rejected.
`WorldState` itself contains only environment, instructions, and time and does not enumerate tool
names: the request's `ToolSpecPlan` schema is the sole model-visible owner of tool availability. The
AutoReview Guardian receives the same tool-inventory-free world-state snapshot and an empty tool
surface; exact action evidence is carried separately.

The AutoReview Guardian receives a complete typed action-evidence object separately from the bounded
human-facing permission preview. MCP calls retain their normalized full arguments, configured target,
exact tool name, and credential-presence flag; Docling retains its exact endpoint, local path or source
URL, effective format/OCR/image/page options, and credential-presence flag. Secret values are not sent.
If redaction or invalid configuration makes the executable effect incomplete, AutoReview denies before
calling either the Guardian or a human. The Guardian request includes the current `WorldState`, bounded
active canonical task context, the current exact committed response/call, and bounded results of prior
tools in that same response. It has no tools or continuation, sends no sampling/thinking override,
accepts host-provided reasoning as non-authoritative transport output, and uses the turn-captured client-side
`model.request_timeout_ms` as its absolute total deadline without sending that limit to the host.
Guardian transport admission is derived from the turn-captured canonical `ProviderTarget.api_mode`,
not the connection-profile name. All four current profiles therefore use their exact tool-less wire:
`lm_studio` and `openai_responses` use Responses, while `openai_compatible` (including oMLX) and
`lm_studio_chat_completions` use Chat Completions. Both wires omit tools, continuation, sampling,
reasoning/output overrides, and arbitrary extra body. A future unknown wire fails before Guardian
provider contact and never falls back to human confirmation or Ask mode. This serializer/admission
contract does not by itself qualify a concrete host; oMLX operational compatibility requires separate
actual UAT against the configured endpoint and model.

Desktop binds an access update to the current root session and exact runtime epoch. Within the same
epoch, natural `root:N` to `tree:N`/`idle:N` and `tree:N` to `root:N`/`idle:N` settlements are accepted;
an idle-to-active transition, a new epoch, or another session/workspace/config owner is rejected. For a new
TUI root session, `RunSessionAccessModeAdoption` commits the latest pre-admission F8 selection to the
durable session before `SessionStarted` or the agent loop. Switching with a human prompt already pending
does not alter or settle that prompt; it affects only the next permission decision.

Canonical protocol history is the delivered conversation source of truth. A new user turn enters it
directly. An active-turn steer is first accepted into the durable turn-input queue and enters history
at the next safe model-request boundary with the same stable identity. If no further request is made,
a non-interrupted terminal drains the accepted steer into history before finishing; an interrupted
terminal records the interruption and discards the still-pending steer instead.
assistant messages, raw tool calls/outputs, collaboration-mode instructions, and compaction lineage are
stored as typed items. Each Rust history envelope has one `HistoryScope`: `Turn { turn_id }` for
user/steer, assistant/tool, compaction, and delivered mail, or `Session` for collaboration mode and
retained migrated session state. Newly accepted idle mail remains in the durable mailbox and is absent
from canonical history and export until an admitted turn delivers it. SQL stores that enum as a checked
`scope_kind` plus nullable `turn_id`; it never invents a turn ID for session state. A canonical tool call preserves the provider's `tool_name` and
`arguments_json` strings; typed-name parsing, JSON parsing, and schema validation are transient
execution steps. Assistant text and every raw tool call from one provider response share a
`ModelResponseId` and commit in one database transaction before any tool executes, so a partial
response cannot remain or be rewritten to `Invalid` / `null` when parsing fails.
Tool result title, metadata, output, and error live only in canonical `ToolOutput`; the tool sidecar
keeps lifecycle, truncation-path, and timestamp data. Committed durable events are published only
after their storage transaction; streaming deltas and reasoning summaries use a separate runtime-only
path and are not persisted as conversation fragments. A typed turn terminal's discriminated
`outcome` is the only owner of `completed`, `interrupted { cause }`, or `failed { error }`; session
status, finish reason, cause, and display summary are derived from it. Final response identity,
counts, and metrics travel in the same terminal value, and `RunSummary` hands that value across the
runtime boundary instead of restating its fields. Non-turn control commands do not synthesize a
successful turn terminal.
Durable runtime feedback has one typed severity, category, and public message; that same payload is used
for live display, canonical history, turn projection, reopen, and Markdown export. A transient
`RuntimeNotice` remains live-only. Desktop tool progress selects failure/declined items first and then
the newest work, with at most eight items, 2,000 total characters, and 180 characters per line; the
complete evidence remains available by jumping to canonical history or exporting Markdown. Session
usage is recomputed from all canonical `TurnTerminal` runtime events, counting terminal turns and
usage-measured turns separately and never presenting missing usage as zero.
Protocol writes are limited to their atomic session/runtime owners. The generic protocol query/fork
surface cannot append arbitrary event bundles, and the runtime recording sink accepts only its explicit
projection allow-list rather than duplicating model/tool/file/terminal ownership.
TUI does not insert a submitted user/steer row or clear the composer optimistically. It tracks root-run
and steer submission identities, projects a new-user row after durable `UserTurnStored`, and shows an
accepted active-turn steer as a separate pending input rather than a transcript row. Delivery replaces
that pending projection with one canonical user row carrying the same stable identity. It clears only a
draft whose revision and text are still unchanged. A
pre-admission/storage failure or a post-submit edit keeps the draft and creates no phantom user row.
For a new root session, a pre-admission F8 access-mode change is adopted into that durable session before
`SessionStarted` and before the agent loop; F8 during an existing human permission prompt leaves the
prompt unchanged and applies the committed mode only to the next permission decision.
Prompt Enhance is single-flight under a request ID and cancellation token. During the request, `Esc`
cancels the provider while keeping the raw composer and the TUI running; `Ctrl+Q` cancels the provider
and pending review before quitting. A late completion after cancellation cannot reopen the review.

Durable run admission commits the run identity, turn identity, and lease together, so there is no
persisted state where a run owns the session without an active turn. One typed decoder validates the
session status/run/turn/lease quartet for every reader and mutation; partial IDs, non-positive leases,
and impossible idle/running owners fail closed. The same typed storage validator receives the session row
and exact-terminal count/payload from one SQL statement for single-session/list/projection/project/tree
reads, and receives same-transaction evidence for active-admission writes. `running` plus a terminal, or a
terminal status with a missing, duplicate, or status-mismatched
exact terminal, is corruption; admission, renewal, release, and expired replacement cannot normalize it
by clearing the owner. A turn ID is one-shot within its session: admission
rejects it when any canonical history, turn item, runtime event, append-order, or sequence-allocation
trace already exists. Project and Agent Tree gates decode every potentially invalid runtime candidate
before returning a remembered blocker, so a later corrupt row is not hidden; unknown persisted access
modes fail closed instead of becoming `default`. Stop and recovery capture the observed admission plus
turn as an opaque terminal target. A lease renewal by that same owner remains valid, while a replacement
run/turn cannot be terminalized through the stale target. If renewal observes a terminal, it returns the
requested turn's exact typed terminal from the same transaction instead of issuing a follow-up lookup.
User-turn bundles and `RunSummary` terminals
must also match the admitted session/turn identity. Session rollback, filtered fork, expired-run
recovery, and active mail-versus-terminal settlement each have one atomic storage/admission boundary.
In particular, mail acceptance appends only the bounded durable mailbox; it does not append canonical
history or rely on a process-local body copy. Safe delivery atomically changes one pending mailbox row
to delivered and creates the Turn-scoped history, turn item, and runtime event with the same stable ID.
Required direct-child results block an owner terminal until delivered. Ordinary mail arriving after a
visible final can remain pending for the next turn, while stop fences settle mail that must not survive.
Capacity rejection creates no mailbox row, history row, or local wake.

Desktop and TUI use bounded latest/offset canonical snapshots with a fence instead of eagerly loading
the whole history. Desktop can prepend adjacent older turn chunks into one continuous in-memory range,
reprojects turn boundaries after each merge, and keeps latest live/current refreshes under one
latest-wins owner so delayed snapshots cannot roll the transcript or terminal status backward.
Explicit Markdown export reads bounded pages and checks the append fence before it returns a complete
export. Runtime delivery uses bounded mailboxes with explicit backpressure. Accepted-but-unsampled
active steer content exists only in the durable turn-input queue and is not exported as conversation
history; after atomic delivery it is read from canonical history like any other user input. The
process-local wake-up is a coalesced generation signal that carries neither content nor an item
identity, and `wait_agent` also checks the durable queue so another process cannot strand input behind
the local signal. Best-effort harness recording disables only itself when initialization or writing
fails; it does not override the user-visible run/event result.

The V33 migration included in v0.8.0 losslessly backfills the legacy message graph into ordered canonical
protocol items before dropping the legacy tables. V37 converts a raw tool call only when a missing
provider-response identity can be recovered uniquely from canonical evidence in the same turn. With
zero or multiple candidates, the entire upgrade transaction rolls back and leaves the database
unchanged; it neither deletes the ambiguous turn nor introduces an unresolved current payload variant.
Back up the moyAI data directory before upgrading existing data. V38 historically mapped the then-retired
`auto_review` session value one way to `default` and rebuilt that schema's storage domain with only
`default` and `full_access`.
V39 rewrites legacy terminal JSON into the discriminated outcome contract, removes retired durable
retry/delta rows, and fails closed rather than inventing an interruption cause. V40 keeps only valid
flat root-to-direct-child spawn edges; nested edges are discarded without reparenting, while their
child session rows remain as independent sessions. V41 introduced the indexed latest
collaboration-mode lookup. V42 rebuilds canonical history with typed Turn/Session scope, converts old
mode pseudo-turns and terminal-less mail-only pseudo-turns with known projections into append-ordered
session state, and fails the whole migration on an unknown projection. V43 indexes durable truncation-
path ownership for exact bounded maintenance checks. Each maintenance tick advances process-local
`ReadDir` cursors shared across store clones instead of materializing all owners or entries, with at
most 64 live candidates across both namespaces and at most those 64 quarantine renames. Live and
quarantine roots must retain a stable, non-link identity inside the canonical data root; Windows
reparse points, including junctions, fail closed. Orphan harness directories are matched by
both run ID and artifact root, while truncation files use the indexed exact path owner. Both are
atomically detached into a same-volume maintenance quarantine under the producer fence. Destructive
operations never re-resolve the enumerated string path: Windows binds rename/delete to the same
opened entry handle and a stable destination-directory handle, while Unix uses no-follow stable
directory descriptors and single-component relative operations with an immediate identity check.
After the fence is released, a shared `ReadDir` frame stack drains that quarantine without recursive
bulk deletion, keeping filesystem entries examined plus mutation attempts within 64 per tick.
Current-schema opens validate only bounded schema shape; the full payload audit remains part of the
migration cutover.
V44 adds a partial unique index that permits exactly one terminal runtime event per session/turn.
Migration rolls back without recording its marker when duplicate terminals already exist, and current
opens validate the index table, key order, and predicate. Terminal readers also detect a second row and
fail closed rather than relying on the index alone.
V45 restores the current three-value session access domain: `default`, `auto_review`, and `full_access`.
Values already collapsed to `default` by V38 cannot be distinguished from genuine Default choices and
are therefore not reconstructed; users can explicitly select Auto Review again after the upgrade.
V46 upgrades recoverable stored v1 compaction rows to the `user_anchored_checkpoint` layout by
reconstructing bounded real-user anchors from canonical append order. Rows whose real-user text cannot
be recovered remain explicit `legacy_prefix` checkpoints without changing their effective ordering.
The migration validates JSON, hashes, session-local replacement lineage, and anchor bounds, rewrites
compaction rows in bounded pages, and rolls back without its marker when validation fails.
V47 is the current spawn-edge schema. It preserves the flat edges that survived historical V40, then
allows recursive Sub Agent lineage while validating each canonical `/root/...` path against its
immediate parent. It also prevents deletion that would orphan descendants and bounds each retained
tree at 256 agents including the root. Nested edges discarded by V40 cannot be reconstructed.
V48 added durable OwnerResume requests and deferred completion receipts for early success or
recoverable crash failure. Existing early-success rows remain readable for compatibility; current
runtime creates deferred receipts only for crash recovery. V49 adds durable tree-stop fences so
explicitly stopped subtrees, causes, and root boundaries cannot be resurrected after restart. V50
moves `NEW_TASK`, `MESSAGE`, and `FINAL_ANSWER` into the bounded durable mailbox. Current child
completion is queue-only for its exact direct parent and does not create OwnerResume; delivery
rehomes that exact mailbox identity into Turn-scoped canonical history.
V51 adds the durable active-steer FIFO, pending projection, terminal drain-or-discard rules, and the
durable/final timeout rechecks used by cross-process `wait_agent`. Root, cross-session sources,
ambiguous state, and terminal deferred states without an exact later resolver fail closed.
V52 binds every native harness run to its exact canonical session and turn. Ambiguous, missing,
duplicate, or cross-session backfill fails atomically without leaving the marker or a partial
mutation. V53 adds an immutable claim from each explicit mailbox wake to its recipient session,
admission, and turn; an existing OwnerResume remains bound to its exact claimed turn. Completed and
Failed settlement delivers only that selected wake into the claimed turn, Interrupted settlement
discards only that wake, and later triggers remain pending for a later admission. Current opens
validate both the V53 schema and these identities.

The default tool surface exposes `update_plan` for non-trivial work. Its structured result is a
client-visible plan projection: moyAI does not interpret plan text to select the next tool, end the
turn, trigger compaction, or unlock another tool surface. A durable Plan mode exists internally, keeps
`update_plan`, and hides mutation tools, but no CLI, TUI, or Desktop mode selector is currently
exposed.

For a non-trivial investigation or design, the common base instructions also ask the model to keep
the smallest useful internal evidence ledger: material claims are grouped by their current owner,
linked to a direct consumer or verification, and marked missing, observed, or conflicting. The model
should batch independent reads, stop discovery once every required row has direct evidence, and name
any unresolved source fact. This ledger is ephemeral model working context; moyAI does not save or
interpret it as a completion or quality gate, and does not add a separate request Framer, injected
state delta, or host-owned coverage/convergence score.

At the model policy's 90% working target, moyAI selects model-visible semantic units rather than a
fixed item count. When available, the latest provider-reported total is rehydrated from the durable
turn terminal and combined with only the local items appended after that model response. Otherwise,
the full prepared request uses the same coarse UTF-8-bytes/4 fallback as Codex. Request diagnostics
identify which source was used. One provider response's assistant text,
calls, and settled outputs stay together; no compaction is attempted while a tool response is
unsettled. Summary generation keeps the base instructions and native User / Assistant / tool
structure, appends the C8 evidence-grounded checkpoint prompt as the final User input, and sends no tools or provider
cursor. When possible, its source is the largest oldest semantic-unit prefix whose complete tool-less
request fits within half of the working target while still projecting a useful checkpoint. If no such
prefix exists, moyAI keeps semantic units indivisible and chooses the smallest prefix projected to make
progress; one oversized unit remains whole and fails closed if it cannot be summarized safely. A typed
`context_length_exceeded`, or a context-saturated completion containing reasoning tokens but no answer
text, may retry once with a strictly smaller oldest semantic-unit prefix. The provider/local prompt gap
observed on that attempt remains part of the retry and resumed-request safety check. Only the units in
the successful request enter replacement lineage; there is no semantic map/reduce path.
The exact checkpoint text in `assets/prompts/compaction.md` is the current source-level contract.
Runtime validation proves only that its six required headings occur once, in order, with non-empty
bodies. Evidence grounding and semantic completeness remain model-quality concerns over the native
input; structural acceptance does not claim either one.

The resulting checkpoint retains the newest real User and Steer text inputs in original order
within a conservative 20,000-token budget. One boundary input is middle-truncated instead of being
dropped whole, and the prefixed summary is the final User input; old summaries are never promoted to
anchors. A delegated turn's canonical `NEW_TASK` remains an anchor, while ordinary agent messages and
final handoffs belong in the summary. The exact replacement lineage is committed while original
history remains stored. If cancellation occurs or summarization otherwise fails, history remains
unchanged. A non-empty summary is also rejected when the projected replacement is not smaller or the
projected complete request still reaches the 90% working target. Automatic compaction is attempted at
most once in that turn; below the hard limit the original history continues, and at the hard limit
the run fails explicitly. The working target is 90% of moyAI's configured local context budget and the
Codex-style effective full input limit is 95%; an additional configured overflow margin is applied
only when it keeps the hard limit strictly above the working target. Host-owned output limits do not
reserve input tokens or lower either local context limit.

An active session goal is not declared successful after an arbitrary number of idle continuations. It
continues until the goal state, its token/elapsed budget, cancellation, or a typed terminal provides a
semantic stopping condition.

## Multi-Agent Collaboration

Multi-agent collaboration is available by default and normally exposes these six tools to the model:
`spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `interrupt_agent`, and
`list_agents`. Set `[multi_agent].enabled = false` in Settings or the config file to hide them.

- `mode = "explicit_request_only"` delegates only when the user explicitly requests agents,
  sub-agents, delegation, or parallel agent work. `mode = "proactive"` also lets the model delegate
  bounded work when doing so materially improves speed or quality.
- The `multi_agent_root.md` / `sub_agent.md` assets keep source-aligned Codex role and
  message-lifecycle fragments separate from explicitly labelled moyAI local-model coordination.
  The latter adapts direct-tool invocation to moyAI's flat names and adds delegation, evidence
  handoff, and instruction-authority safeguards; the complete assets are not byte-identical Codex
  prompts. The proactive asset likewise keeps the Codex activation text intact, then labels a local
  adaptation of Codex delegation guidance: a high-level plan separates the immediate blocker kept
  local from concrete, self-contained parallel sidecars; root and coding-child work must not overlap,
  root continues non-overlapping work, waits only when the critical path needs a result, and reviews
  returned patches before integration. These static instructions do not create a runtime gate, fixed
  DAG/stage router, or dynamic behavior-correction layer, and do not claim full Codex runtime parity.
- Every agent retains its normal tools and the six collaboration tools under the same model, mode,
  provider, and configuration filters. Spawning does not move the parent onto a collaboration-only
  surface, and `update_plan` does not unlock workspace tools. If the resolved model does not support
  tools, the request has no tool surface and moyAI omits the role/mode messages that would instruct
  it to call collaboration tools.
- Any agent may spawn another agent. The new task name is joined to the caller's canonical path:
  `/root/task1` spawning `task_3` creates `/root/task1/task_3`. Relative agent references resolve from
  the current agent; canonical absolute paths address agents elsewhere in the same tree.
- Each agent remains responsible for its assigned objective and for integrating children it creates,
  while the model chooses concrete bounded subtasks from current evidence. The host does not create a
  planner DAG or fixed scout/stage router.
- The root retains the task-wide plan, integrates child results, and performs final verification.
  Each child returns a concise handoff with outcome, supporting evidence, intentionally changed
  paths, verification and results, and remaining unknowns or risks. The root uses that handoff as
  working evidence instead of rebuilding private investigation; final verification checks the
  delegated acceptance criteria and resulting workspace state, inspecting only missing or
  conflicting evidence.
- A descendant's newest host-delivered `NEW_TASK` plus later host-delivered parent messages defines
  delegated scope only within system, developer, applicable project/skill, and user instructions.
  Parent-supplied findings and decisions are working context, not higher-priority instructions or
  independently verified facts. Quoted or embedded external content remains data unless a system,
  developer, or user instruction adopts it. The descendant inspects only gaps needed for its scope,
  avoids repeating private grounding, and returns the evidence handoff above.
- `max_concurrent_agents` is the root-inclusive limit for simultaneously active agents. The default
  `4` therefore allows the root plus at most three active descendants anywhere in the tree. The
  internal execution limiter excludes the root and derives those three descendant slots from the
  root-inclusive public value. Completed agents remain listed and available for follow-up work but
  no longer consume an active slot. The retained registry is
  bounded at 256 entries including the root (at most 255 descendants at any depth); once full,
  another spawn is rejected rather than evicting history or reusing a spawn order.
- `max_concurrent_model_requests = 1` keeps local-LLM model requests within the tree serialized by
  default, while agents can still make progress independently around tool and review work. Raise it
  only when the configured inference server can safely sustain parallel requests. Both concurrency
  limits are captured when the retained agent scheduler is first loaded. Later root turns reuse that
  scheduler and model-request semaphore; a different value is rejected before model sampling rather
  than mutating a live tree. Start a new session, or reopen the session in a new process, to use
  different limits.
- `wait_agent` defaults to 30,000 ms, accepts 10,000 through 3,600,000 ms, and returns immediately
  when agent activity or active-turn user input arrives. Callers can request a longer bounded wait
  when the task specifically requires it.
- Each descendant is a separate durable session linked to its immediate parent and tree root. Normal
  project/session lists keep those implementation sessions hidden. `spawn_agent` accepts
  `fork_turns = "all"` (the default), `"none"`, or a positive integer string for only that many recent
  turns. `"all"` streams the parent's active history in bounded pages under a stable append fence and
  copies the currently active user turns, plain final assistant messages owned by successfully
  completed terminals, durable collaboration-mode instruction, and active compaction summary.
  History replaced by that summary is not resurrected, and reasoning, tool traffic, retired control
  state, and permission evidence are not copied. Target-session existence is checked in the same
  transaction; a fence mismatch or mid-copy failure rolls back the entire fork. Sub Agent activity is
  recorded only while its owning root session has a fresh active turn.
- A live agent keeps the configuration, workspace, and permission broker captured for that agent
  execution. Spawn inherits the caller's resources, and a follow-up uses the exact target's retained
  resources; starting a new root turn never rewrites a still-running child. Project/session/workspace
  navigation replaces only the view's workspace-specific run service: the process scheduler, session
  event hub, and active Agent Trees remain the same owners, and each admitted execution keeps its
  exact run service. On process restart,
  lineage rehydration follows Codex's resume boundary: the current root resume configuration,
  workspace, and permission broker are supplied to every restored descendant instead of partially
  rebuilding a child configuration from session columns.
- Spawn, follow-up, ordinary message, and child completion remain typed Agent items and Codex-style
  `NEW_TASK`, `MESSAGE`, and `FINAL_ANSWER` envelopes at the canonical-history boundary. At the final
  OpenAI-compatible adapter, providers without Codex's `agent_message` type receive the preserved
  envelope as a standard `user`-role message. The accompanying logical Developer instruction treats
  that compatibility representation as delegated working context within system, developer, project/
  skill, and original-user constraints. A child's `FINAL_ANSWER` goes to its immediate parent, which receives the concise
  evidence handoff rather than the private investigation transcript. Child-session creation, the recursive edge, the requested
  history fork, and the initial `NEW_TASK` are one transaction. Before admission, a launch failure
  settles that exact trigger as `Failed` and atomically sends one terminal handoff to the immediate
  parent; cancellation settles it as `Interrupted` without a success-like handoff. A follow-up starts
  only its exact target and does not wake an inactive ancestor first. Its durable `trigger_turn`
  intent is distinct from whether storage authorizes an immediate execution. A ready inactive target
  reserves one descendant slot before its pending durable mailbox item is appended; if capacity is
  unavailable, no mailbox row, canonical history, or process-local wake is added. Mail for an active
  target does not consume another slot.
- Like Codex threads, each root or descendant owns its terminal independently of descendant
  liveness. `Completed`, `Failed`, and target-only `AgentInterrupted` neither wait for nor cancel
  descendants. If an answer depends on a child result, the model must call `wait_agent` before
  returning its final response. Permission Abort stops only the requesting execution, and ordinary
  User Stop stops only the exact current root execution. Neither cascades to siblings or descendants.
  Only the separately named explicit tree-stop operation is allowed to stop the retained tree.
- A child terminal creates one durable `FINAL_ANSWER` for its exact immediate parent with
  `trigger_turn = false`; it never bubbles to root or auto-resumes a terminal parent. An active
  parent can receive it at a safe mailbox boundary. If it races a non-interrupted terminal while
  current-turn delivery is still eligible, the terminal writer records it in canonical IAC history
  in the same transaction without another model sample. Mail assigned to the next-turn phase stays
  pending and is available to the parent's next explicit turn. A late child result never rewrites
  the parent's existing terminal.
- Historical V48 `completed_early` rows remain readable and stoppable for storage compatibility,
  but current normal completion never creates them. Deferred completion is current only for
  `crash_failed` recovery.
- A crashed OwnerResume turn re-pends the same request without leaking the crash failure upstream.
  Retry success/failure supersedes the crash receipt, interruption discards it, and repeated crashes
  roll the single pending receipt forward. An explicit follow-up to the crashed owner is instead a
  schedule-ready ExplicitTask and takes precedence over OwnerResume; the same recovery applies when
  the crash has no OwnerResume source. Its retry Completed / Failed terminal supersedes the old crash
  receipt, while Interrupted discards it. Every live current-OwnerResume read and post-admission
  projection shares the mail-delivery fence and authoritatively replaces stale local R1 with durable
  `None` or R2; rollback rejects a turn still named by any OwnerResume claim. Shared startup bootstrap
  restores the exact readiness and performs crash recovery before rehydrating the Agent Tree.
- Every continuation turn receives a fresh run control. Ordinary Stop targets that exact active
  continuation and does not reopen an earlier terminal or cancel detached children. The separate
  explicit tree-stop operation closes the retained tree, settles dormant follow-ups, and discards
  their deferred owner state so a later restart cannot revive explicitly stopped work.
- Desktop coalesces each turn's Sub Agent lifecycle events by `agent_path` into one compact, individually
  clickable stable-icon job with its task preview and latest status inside that turn's collapsible activity group; the root Agent's final response
  remains the next normal assistant message. Activating a job, or the compact summary in Output,
  opens a right pane: the list is grouped by status and each selected child shows its read-only bounded
  canonical execution transcript. Older child execution pages can be prepended in place from that pane;
  only a Running child with an exact projected active turn exposes an interrupt action, which returns
  workspace/root/path/child/turn identity and rejects stale or forged targets.
  each read stays bounded and reprojects the complete loaded range across turn boundaries. It does not navigate to or select the child session, rejects stale
  workspace/root/agent/child responses, and becomes a right-side drawer in narrow
  windows. Permission prompts identify the requesting agent and are serialized. Detached child
  liveness alone does not block new-chat, session, project, or workspace navigation, and a new root
  request can start after the prior root terminal while children continue independently. Desktop
  Stop targets the exact selected root execution; whole-tree cancellation remains a separate,
  explicitly named destructive operation.
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
or Docling health request. Invalid local settings open Initial Setup or Settings; the left-rail
Connection Settings shortcut and top-bar Session Settings entry remain the normal repair routes. Live connectivity is
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
Discovery and individual Skill loading keep their own filesystem limits. The model-visible catalog is a
deterministic complete-entry prefix of the sorted result, capped at 64 entries and 16 KiB; diagnostics
report included and omitted counts, and an oversized entry is never cut into partial instructions.

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

Run `npm run verify:gui -- --suite smoke` for the regular unit/build/actual-GUI phase;
use `--suite regression` for broader coverage. The [GUI automation guide](tests/desktop_e2e/GUI_AUTOMATION.md)
explains prerequisites, existing-binary runs, CI and pending manual review. Automated GUI results
do not approve visual quality, native input or all untested control states.

Published release packages must also pass a visible Desktop GUI manual ST before upload.
Record the result in a UTF-8 Markdown artifact containing `Manual ST Gate: PASS`, then pass that
file through `scripts/package-release.ps1 -ManualGuiStResultsPath ...`; the artifact is copied into
the release zip under `docs/release/manual-gui-st-results.md`.

## Status

moyAI is currently developed and tested primarily on Windows. Its prompt, compaction, and agent-loop
behavior are primarily optimized and behaviorally validated for `qwen/qwen3.6-27b` hosted by LM
Studio, which is also the shipped default and example model. Existing user config files remain
authoritative and are not rewritten when the product default changes.

Other OpenAI-compatible models can be used, but model behavior, tool-use quality, context length, and vision support vary by provider and model.

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
