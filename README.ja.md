<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>ローカル LLM、閉域環境、プライベートなワークスペースのための coding agent。</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.1.0_beta-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.md">English README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0">beta をダウンロード</a>
  ·
  <a href="#quick-start">Quick Start</a>
  ·
  <a href="#configuration">Configuration</a>
</p>

---

## moyAI とは

moyAI は、クラウド前提の開発支援ツールをそのまま使いにくい環境のために作っている、Rust 製の coding agent です。

OpenAI 互換 API を持つローカル LLM サーバーに接続し、ワークスペースの調査、ファイル編集、shell 実行、セッション履歴、検証までを扱います。CLI、TUI、Tauri Desktop App は、同じ Rust core の上で動きます。

目指しているのは、派手なデモではなく、手元の開発作業で普通に頼れる道具です。モデルはローカルに置き、作業の証跡は見える形で残し、何を読んで、何を変えて、何を検証したのかを追えるようにします。

## なぜ作ったか

多くの coding agent は、hosted model、online service、plugin marketplace、常時 internet access を前提にしています。便利な一方で、機密コード、社内ネットワーク、ローカル推論サーバー、再現性が必要な開発環境では、その前提が合わないことがあります。

moyAI は、その制約を正面から扱います。

| 方針 | 内容 |
| --- | --- |
| Local-first | LM Studio などの OpenAI 互換 local LLM endpoint に接続します。 |
| Workspace-aware | project を検索し、読み、編集し、patch し、検証します。 |
| Evidence-oriented | transcript、file changes、tool output、session history を後から確認できます。 |
| GUI and terminal | Desktop、CLI、TUI を同じ Rust core で使えます。 |
| Closed-network friendly | 配布先端末では npm、Rust toolchain、internet、dev server を要求しません。 |

## Highlights

- Project chat、Quick Chat、Transcript、Artifact pane、Settings、Provider discovery を持つ Tauri Desktop App。
- terminal 派向けの CLI / TUI。
- OpenAI 互換 local LLM への接続と model availability check。
- `/v1/models` と LM Studio `/api/v1/models` からの model metadata discovery。
- workspace search、directory inspection、guarded file read、diff-based edit、shell execution。
- permission preset は `default`、`auto_review`、`full_access` の 3 種類。
- vision-capable model では画像添付に対応。
- Docling Serve / HTTP MCP と連携した document workflow。
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、`.moyai/commands/*.md`、local `SKILL.md` を読み込み。
- protocol-first session history、Markdown export、deterministic preflight / harness command。

## Current Release

最初の beta release はこちらです。

[**moyAI v0.1.0 beta release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.1.0)

Windows 向け release zip には、次のものが入っています。

- CLI / TUI 用の `bin/moyai.exe`
- Desktop App 用の `bin/moyai-desktop.exe`
- bundled `ui/desktop-web/dist/` assets
- README、LICENSE、release notes、config example、manifest、SHA256 checksum

利用先の Windows 端末に、npm、Rust toolchain、internet access、local web dev server は不要です。

## Quick Start

1. LM Studio などで OpenAI 互換 local LLM server を起動します。
2. release zip をダウンロードして展開します。
3. `bin/moyai-desktop.exe` を起動します。
4. `LLM URL` で base URL と model を設定し、model discovery を確認します。
5. Quick Chat を使うか、project workspace を選んで開発チャットを始めます。

CLI から使う例です。

```bash
moyai run --dir /path/to/workspace "このプロジェクトの主要モジュールを調べて要約してください。"
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

開発用 build:

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

既定では、release artifact は repository の外側にある `project_sandbox/releases/` に出力されます。

## Configuration

moyAI は、ユーザー全体で共有する 1 つの config file を読み、その上に environment variables と CLI overrides を重ねます。

Windows の既定 config path:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

release folder や workspace folder に専用の config file を置く必要はありません。Desktop、TUI、CLI は同じ user-wide settings を読みます。

設定例:

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

よく使う environment variables:

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

`moyai-desktop.exe` の cold start では、moyAI splash を最低 5 秒表示し、その間に次を確認します。

- global config file の状態
- workspace availability
- configured provider / model catalog
- Docling enabled 時の Docling Serve `/health` / `/ready`

設定が足りない場合や接続確認が通らない場合は、メインウィンドウを開いたあとに Settings または LLM URL を自動で表示します。

## Project Instructions

moyAI は repository local の instruction を読み込みます。

- `AGENTS.md`
- `CLAUDE.md`
- `.moyai/rules`
- `.moyai/rules-<route>`
- `.moyai/commands/*.md`
- `.moyai/skills/**/SKILL.md`

外部 plugin marketplace に依存せず、project ごとの運用ルールを repository 内で管理できます。

## Verification

手元でよく使う check です。

```bash
cargo fmt --all --check
cargo check
cargo test --lib
cargo test --tests
cargo run --bin moyai -- preflight run
```

## Status

moyAI は現在、主に Windows で開発・検証しています。主な検証構成は、LM Studio でホストした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版です。

OpenAI 互換 model であれば他の model も利用できますが、tool-use quality、context length、vision support、応答速度は provider / model によって変わります。

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
