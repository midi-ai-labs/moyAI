<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>A local-first coding agent for private workspaces, local LLMs, and closed-network development.</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.8.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.8.0-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.ja.md">日本語 README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.8.0">Download release</a>
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
- Desktop Stop validates the projected workspace, root session, run generation, and Agent Tree epoch, so stale UI actions cannot cancel a later run. Settings values, baseline, dirty state, and monotonic revision exist only in one frontend-local draft owner. Rust projects typed clean/dirty capability variants and statelessly validates a complete draft plus a decimal-string config-generation target before Apply, Save, Reset, or another config-owner mutation. Commit builds one complete temporary `ResolvedConfig`, preserving cleared optional values instead of re-layering them. Active-turn steer clears input only after durable acceptance.
- CLI and TUI for terminal-centered workflows.
- OpenAI-compatible local LLM connection with explicit model availability diagnostics. moyAI connects to the configured external HTTP endpoint; it does not launch or supervise the provider process.
- Evidence-first task planning with canonical `update_plan` as a client-visible progress projection, not an execution gate.
- One immutable `ResolvedTurnConfig`/turn/step context captured at admission, canonical protocol history, and atomic response-scoped assistant/raw-tool-call commits keyed by `ModelResponseId`.
- LM Studio Responses API support with turn-scoped `previous_response_id` continuity and typed reasoning summaries.
- Automatic LLM semantic compaction near the context threshold, using response/call-output semantic units, a prepared-request token target, giant-item map/reduce, replacement lineage, and non-destructive no-progress handling.
- LM Studio metadata discovery through `/v1/models` and `/api/v1/models`.
- Bounded workspace traversal/search/directory inspection with continuation cursors, guarded file reads, diff-based edits, and shell execution.
- File writes and patches use one stable-handle, no-clobber conditional commit for create, update, delete, and rollback. A concurrent external replacement wins without being overwritten; if restoration cannot reclaim the target name, moyAI reports the preserved backup path. Parent directories are not created implicitly, so create the parent first.
- On Unix, moyAI cannot prove that a writable descriptor opened before an update or delete no longer references the detached inode. Creation remains unchanged, but an existing-file update installs the new target and a delete detaches the target while retaining the old inode at a private backup path; both report a typed partial-commit error instead of claiming safe cleanup. Inspect and reconcile the reported backup because a pre-opened writer can still modify it.
- Permission modes: **Ask for approval** (`default` / 承認を求める), **Approve for me** (`auto_review` / 代理で承認), and **Full access** (`full_access` / フルアクセス). Ask and Auto share one deterministic admission policy and the same Windows `workspace-write` restricted-token/ACL profile; explicit `sandbox_permissions: "require_escalated"` plus `justification`, or a detected destructive/network/external/authority effect, goes to a human in Ask or a separate tool-less AI Guardian in Auto. The Windows backend identity-pins admitted roots and selected existing authority carveouts, content-pins protected regular files, gives each launched process/thread an explicit system-only descriptor, inherits only stdio, applies Job process-tree/UI restrictions before resume, and fails closed without an unrestricted retry. This unelevated profile is a finite existing-object defense, not a complete Windows namespace or Codex-enforcement equivalent: absent authority names, unrelated nested instruction files, protected descendants with overriding explicit/inheritance-disabled DACLs, uninspected outside paths, direct sockets, same-user host-process memory, and same-desktop synthetic input remain residuals. Its ACL preflight can propagate through existing trees synchronously and is not covered by the child timeout. Full Access and an approved process elevation run `Unrestricted` as the current user, so their child filesystem mutations do not pass through typed file guards; typed `write`/`apply_patch`, MCP/Docling, and process lifecycle checks keep their own guards. A committed mode change affects the next permission decision, while a pending request and an admitted effect retain their original decision/profile. Native process sandboxing is currently Windows-only; workspace-mode process effects fail closed elsewhere. A future elevated dedicated-identity/firewall/private-desktop backend is required for the hard boundary.
- Vision-capable model support for image attachments.
- Optional Docling Serve and HTTP MCP integration for document-heavy workflows.
- Local instructions from `AGENTS.md`, `CLAUDE.md`, `.moyai/rules*`, `.moyai/commands/*.md`, and local `SKILL.md` files.
- Canonical protocol session history, typed turn terminals, Markdown export, and lightweight live-smoke artifacts.
- Optional root-scoped multi-agent collaboration with separate child sessions and visible Desktop activity.

## Current Release

The current beta release is available here:

[**moyAI v0.8.0 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.8.0)

v0.8.0 includes the canonical runtime/storage cutover, Codex-style planning, Responses transport,
semantic compaction, and the Desktop interaction hardening described in this source tree.

The Windows release zip includes:

- `bin/moyai.exe` for CLI / TUI workflows
- `bin/moyai-desktop.exe` for the Desktop app
- `bin/moyai-cleanup.exe` for resetting user-wide moyAI AppData to first-run state
- bundled `ui/desktop-web/dist/` assets
- README files, license, release notes, config example, getting-started guide, and in-package SHA256 checksums

The GitHub Release publishes the zip together with its external manifest and zip SHA256 sidecar.

On the target Windows machine, you do not need npm, the Rust toolchain, internet access, or a local web dev server.

## Quick Start

1. Start, or connect to, an OpenAI-compatible LLM server reachable at the configured HTTP URL.
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
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 0.8.0 -ManualGuiStResultsPath path\to\RESULTS.md
```

Run packaging from the clean source commit for that release. If `v<version>` already exists, the
script permits a publishable rebuild only from the commit identified by that tag; use a newly
synchronized version for later source.

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
provider_api_mode = "responses"
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
access_mode = "default"

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

`request_timeout_ms` is one response-start operation budget shared by connection attempts, connection
retry delays, request-body upload, and waiting for response headers. `stream_idle_timeout_ms` limits a period with no SSE
event after streaming starts. Both default to 300,000 ms. They are no-progress deadlines, not a cap on
total generation time; explicit config or environment overrides remain supported.
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
Desktop keeps in-progress Settings values, their baseline, dirty state, and monotonic revision in one
frontend-local draft owner; Rust keeps no field-value, dirty, or revision mirror. Rust projects typed
clean and dirty semantic-capability variants, and the frontend selects the variant matching its local
dirty state while adding only local single-flight gates. Apply, Save, and Reset send a complete stable
key/value draft with the workspace/session/config-generation target. Access, Provider Apply/Save, and
Import send the same complete draft with their owner target. Rust statelessly validates draft
completeness, the current effective baseline, and admission before any side effect. Config generation
crosses the Rust/TypeScript boundary as an exact `u64` decimal string, never a JavaScript number.
Apply builds one complete temporary `ResolvedConfig`, so a cleared optional field remains absent
instead of inheriting a stale global/base value. Global Save separately merges only dirty fields into
the current TOML document. Only a correlated success matching the latest local revision and target
clears the frontend draft; a stale async response cannot mutate or clear a different draft.

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
provider policy owns only API mode and reasoning transport. Metadata mode selects exactly one
declared metadata endpoint, while `provider_api_mode` separately selects the generation wire
encoding. Availability is a metadata-only, explicit diagnostic; it does not run tool/vision
generations or mutate product capability config.
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
The Tauri Desktop `LLM URL` overlay exposes the same mode switch beside the provider URL and model list.
It also owns `context_window` and `max_output_tokens` inputs so vLLM/vLLM-MLX limits can be managed
inside moyAI instead of relying on shell environment variables. Current vLLM-MLX `/health` and
`/v1/status` responses expose the hosted model name, but not the server startup `--max-tokens` /
`--max-request-tokens` values, so moyAI auto-detects the model and keeps request limits as managed
config unless a provider exposes those fields in `/v1/models`.

`provider_api_mode = "responses"` is the default generation transport and posts to `/v1/responses`.
Choose `provider_api_mode = "chat_completions"` explicitly for a provider that requires
`/v1/chat/completions`. The retired string `auto` is accepted only at the config/serde input boundary
and normalized one way to `responses`; it is not a runtime mode and metadata mode no longer changes
the generation transport implicitly. The Responses transport keeps provider state within the active
turn by reusing `previous_response_id` and sending only new tool outputs or steer input after a
completed response. Raw reasoning text is neither replayed nor stored as assistant context. A
requested typed reasoning summary is a runtime-only client event, not a durable conversation or
runtime row.

Every generation request has a runtime-only provider request ID and reports the phases
`attempt_started`, `request_in_flight`, `headers_received`, `first_progress`, `last_progress`, and
`provider_terminal`, plus attempt/elapsed data and a sanitized endpoint. These are transport
boundaries observed by moyAI; they do not infer provider-process startup, server-side acceptance, or
model-instance loading. A long `request_in_flight` phase establishes only that the operation has not
reached response headers. Before POST, moyAI bounds messages, tools, schemas, extra body, stop data,
images, and the exact serialized wire bytes. After headers, it also bounds raw stream bytes, events,
tool calls, arguments, idle time, and absolute stream duration.

Reasoning controls are optional. A reasoning-capable model can use, for example,
`reasoning_effort = "medium"` and `reasoning_summary = "concise"`. Responses has a standard typed
contract. Chat Completions varies by provider, so reasoning parameters remain fail-closed unless
`chat_completions_reasoning_parameters = "effort_only"` or `"effort_and_summary"` is configured.

## Runtime and History Continuity

Each turn captures one complete `ResolvedTurnConfig` for model, provider target, operation deadlines,
the admitted permission preset, and remaining effective settings, then gives its single `TurnContext`
owner the turn/admission identity, selected policy, and durable collaboration-mode instruction. Partial
configuration is resolved only before admission and is not merged again by later runtime stages.
It also captures one turn-start wall-clock snapshot. Step/world-state refreshes reuse that snapshot so
a clock tick alone does not invalidate Responses continuity; an explicit `current_time` tool call still
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

The AutoReview Guardian receives a complete typed action-evidence object separately from the bounded
human-facing permission preview. MCP calls retain their normalized full arguments, configured target,
exact tool name, and credential-presence flag; Docling retains its exact endpoint, local path or source
URL, effective format/OCR/image/page options, and credential-presence flag. Secret values are not sent.
If redaction or invalid configuration makes the executable effect incomplete, AutoReview denies before
calling either the Guardian or a human. The Guardian request includes the current `WorldState`, bounded
active canonical task context, the current exact committed response/call, and bounded results of prior
tools in that same response. It has no tools, reasoning, or continuation, does not inherit task-generation
sampling/stop/arbitrary-extra-body controls, and has a 90-second total deadline.

Desktop binds a mode update made while only child agents remain active to the current root session and
the exact `tree:N` owner; only the matching completion from `tree:N` to `idle:N` is accepted. For a new
TUI root session, `RunSessionAccessModeAdoption` commits the latest pre-admission F8 selection to the
durable session before `SessionStarted` or the agent loop. Switching with a human prompt already pending
does not alter or settle that prompt; it affects only the next permission decision.

Canonical protocol history is the conversation source of truth. User and steer turns enter it directly;
assistant messages, raw tool calls/outputs, collaboration-mode instructions, and compaction lineage are
stored as typed items. Each Rust history envelope has one `HistoryScope`: `Turn { turn_id }` for
user/steer, assistant/tool, compaction, and mail delivered to an active turn, or `Session` for
collaboration mode and mail delivered while no turn is active. SQL stores that enum as a checked
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
Protocol writes are limited to their atomic session/runtime owners. The generic protocol query/fork
surface cannot append arbitrary event bundles, and the runtime recording sink accepts only its explicit
projection allow-list rather than duplicating model/tool/file/terminal ownership.
TUI does not insert a submitted user/steer row or clear the composer optimistically. It tracks root-run
and steer submission identities, projects the row after durable `UserTurnStored` / successful
`SteerStored` acceptance, and clears only a draft whose revision and text are still unchanged. A
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
In particular, mail committed first is drained in the same turn, while a terminal committed first
prevents a later active-recipient append. Mail for an idle recipient is history-only session state: it
creates no runtime event, turn item, or terminal, remains visible to the next turn and Markdown export,
and is not consumed by rollback of a real turn.

Desktop and TUI use bounded latest/offset canonical snapshots with a fence instead of eagerly loading
the whole history. Explicit Markdown export reads bounded pages and checks the append fence before it
returns a complete export. Runtime delivery uses bounded mailboxes with explicit backpressure. Active
steer content is read only from canonical history through append-position cursor pages of at most 200
items; the process-local wake-up is a coalesced generation signal that carries neither content nor an
item identity. Best-effort harness recording disables only itself when initialization or writing
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
- The first release uses one flat `/root/<task>` namespace: only the root may call `spawn_agent`, every
  child is linked directly to that root, and a child cannot spawn another Sub Agent.
- `max_concurrent_agents` is the root-inclusive limit for simultaneously active agents. The default
  `4` therefore allows the root plus up to three children to run at once. Completed agents remain
  listed and available for follow-up work but no longer consume an active slot. The retained registry
  is bounded at 256 entries including the root (at most 255 direct children); once full, another spawn
  is rejected rather than evicting history or reusing a spawn order.
- `max_concurrent_model_requests = 1` keeps local-LLM model requests within the tree serialized by
  default, while agents can still make progress independently around tool and review work. Raise it
  only when the configured inference server can safely sustain parallel requests.
- Each child is a separate durable session linked directly to its root. Normal project/session lists keep
  those implementation sessions hidden. `spawn_agent` accepts `fork_turns = "all"` (the default)
  or `"none"`; `"all"` streams active history in bounded pages under a stable append fence and copies the currently active user turns, visible assistant messages, durable
  collaboration-mode instruction, and active compaction summary. History replaced by that summary is
  not resurrected, and reasoning, tool traffic, retired control state, and permission evidence are not
  copied. Target-session existence is checked in the same transaction; a fence mismatch or mid-copy
  failure rolls back the entire fork. Sub Agent activity is recorded only while its owning root session has a fresh active turn.
- Every continuation turn receives a fresh run control, while the Stop target remains the retained
  root Agent Tree. A completed turn's terminal classification is not reopened for the next turn: a
  Stop that wins first blocks continuation, and a continuation that wins first is stopped as part of
  the same tree.
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
