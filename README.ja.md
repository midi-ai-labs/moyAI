<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="640">
</p>

# moyAI

**moyAI** は、閉域環境、プライベートコード、ローカル LLM 運用を前提にした Rust 製のローカルファースト coding agent です。

OpenAI 互換のローカル LLM サーバーへ接続し、ワークスペース内の検索、読み取り、編集、shell 実行、セッション履歴、検証を扱います。CLI、TUI、native desktop app は同じ Rust core の上で動きます。

[English README](README.md)

## なぜ moyAI か

多くの coding agent はクラウド利用、外部ネットワーク、hosted model、online plugin ecosystem を前提にしています。しかし、機密コード、閉域ネットワーク、ローカル推論サーバー、再現性を重視する社内開発環境では、その前提が合わないことがあります。

moyAI は、そのような環境で実用的に使うために作られています。ローカル実行、明示的な設定、ワークスペースを理解した tool 実行、永続的な session history、実行内容を後から確認できる deterministic harness を重視しています。

## 主な機能

- OpenAI 互換 API による local LLM 接続。
- `/v1/models` と LM Studio `/api/v1/models` metadata による model discovery。
- CLI、TUI、Slint native desktop app。
- workspace search、directory inspection、file read、diff-based edit、shell execution。
- large file、binary、model checkpoint、structured document に対する read guard。
- 閉域 Docling Serve と HTTP MCP server 連携。
- protocol-first history による session persistence と Markdown export。
- vision-capable model への画像添付。
- `default`、`auto_review`、`full_access` の permission preset。
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、local `SKILL.md` の instruction loading。
- `.moyai/commands/*.md` による reusable workflow command。
- uncommitted changes / branch comparison の review entrypoint。
- runtime contract を検査する deterministic preflight / harness command。

## 現在の状態

moyAI は、closed-network / local-LLM 前提の実用 coding agent として開発中です。この repository には core runtime、CLI、TUI、desktop app、session storage、tool execution layer、provider metadata probing、harness / preflight infrastructure が含まれます。

## 現在の想定環境

moyAI は現在、Windows 環境で利用することを前提に開発・検証しています。

開発時点では、LM Studio でホスティングした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版に最適化しています。その他の model や provider profile への対応は今後の開発予定です。

## 必要なもの

- Rust 2024 edition に対応した Rust toolchain。
- OpenAI 互換の local または到達可能な LLM endpoint。
- 任意: model metadata discovery 用の LM Studio。
- 任意: document workflow 用の Docling Serve または HTTP MCP server。

## Build

```bash
cargo build
```

## Run

```bash
cargo run -- run --dir /path/to/workspace "このプロジェクトの主要モジュールを調べて要約してください。"
```

binary を install 済みの場合:

```bash
moyai run --dir /path/to/workspace "parser のテストを追加してください。"
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

release build では CLI / TUI 用の `moyai.exe` と、Desktop App を直接起動する `moyai-desktop.exe` が生成されます。Windows では `moyai-desktop.exe` をダブルクリックすると Desktop App が開きます。Desktop runtime window / taskbar icon は `logo/fabicon/android-chrome-512x512.png` を使い、Windows executable resource は multi-size の `logo/fabicon/moyai_app_icon.ico` を使います。
workspace 未指定時、Desktop App は現在の Windows user の Desktop folder を default workspace として開きます。

## 設定

moyAI は global config、workspace config、environment variables、CLI overrides から設定を読みます。

通常の app 初回起動時に、global config が存在しない場合は編集可能なデフォルト値入りの設定ファイルを自動生成します。

- `%APPDATA%\midi-ai-labs\moyai\config\config.toml`

release folder に `.toml` を同梱する必要はありません。binary は `C:\tools\moyai\` のような安定した install directory に配置し、user config と session data は Windows user profile 配下に保存します。

workspace config:

- `moyai.toml`
- `.moyai/config.toml`

主な environment variables:

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

設定例:

```toml
[model]
base_url = "http://127.0.0.1:1234"
# LM Studio でホスティングした qwen3.6-35b-a3b、lmstudio-community 版。
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

moyAI は repository local の instruction を読み込みます。

- `AGENTS.md`
- `CLAUDE.md`
- `.moyai/rules`
- `.moyai/rules-<route>`
- `.moyai/commands/*.md`
- `.moyai/skills/**/SKILL.md`

外部 plugin marketplace に依存せず、project ごとの運用ルールを repository 内で管理できます。

## Verification

代表的な local check:

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
