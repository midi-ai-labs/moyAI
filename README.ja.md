<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="640">
</p>

# moyAI

**moyAI** は、ローカル LLM と閉域環境での利用を前提にした Rust 製の coding agent です。

OpenAI 互換 API を持つローカル推論サーバーにつなぎ、ワークスペースの調査、ファイル編集、shell 実行、セッション履歴、検証までを扱います。CLI、TUI、Desktop App は同じ Rust core の上で動きます。

[English README](README.md)

## どんな用途向けか

クラウド前提の coding agent は便利です。ただ、機密コード、社内ネットワーク、ローカル推論サーバー、再現性が必要な開発環境では、そのまま入れにくいことがあります。

moyAI は、そうした環境でも普段の開発道具として使えることを目指しています。設定は明示的に管理し、作業履歴はローカルに残し、agent が何を見て、何を編集し、何を検証したかを後から追えるようにしています。

## できること

- OpenAI 互換 API 経由で local LLM に接続します。
- `/v1/models` と LM Studio の `/api/v1/models` から model metadata を読みます。
- CLI、TUI、Tauri Desktop App を同じ core で使えます。
- workspace search、directory inspection、file read、diff-based edit、shell execution を扱います。
- large file、binary、model checkpoint、structured document には read guard をかけます。
- 閉域 Docling Serve と HTTP MCP server を使った document workflow に対応します。
- protocol-first history による session persistence と Markdown export ができます。
- vision-capable model では、CLI / Desktop から画像を添付できます。
- permission preset は `default`、`auto_review`、`full_access` の 3 種類です。
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、local `SKILL.md` を読み込みます。
- `.moyai/commands/*.md` で project local な workflow command を定義できます。
- uncommitted changes や branch comparison の review entrypoint を使えます。
- runtime contract を確認する deterministic preflight / harness command を実行できます。

## 現在の開発状況

moyAI は、closed-network / local-LLM 前提で実用できる coding agent として開発しています。現在の repository には、core runtime、CLI、TUI、Desktop App、session storage、tool execution layer、provider metadata probing、harness / preflight infrastructure が入っています。

主な検証環境は Windows です。開発中の標準構成は、LM Studio でホストした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版です。ほかの model や provider profile も扱えるようにしていますが、まずはこの構成での安定性を重視しています。

## 必要なもの

- Rust 2024 edition に対応した Rust toolchain
- OpenAI 互換 API を提供する local または到達可能な LLM endpoint
- 任意: model metadata discovery 用の LM Studio
- 任意: document workflow 用の Docling Serve または HTTP MCP server

## まず試す流れ

1. LM Studio などで OpenAI 互換 API を起動します。
2. `moyai-desktop.exe` を起動します。
3. `LLM URL` で base URL を入れて、model を読み込みます。
4. まずは Quick Chat で短い質問を送ります。
5. code を触らせるときは、Project から workspace folder を選んで依頼します。

設定や model が足りない場合は、起動後に Settings または LLM URL が自動で開きます。そこで base URL と model を確認してください。

## Build

CLI / core だけなら、通常の Rust project と同じです。

```bash
cargo build
```

Desktop UI は Tauri + TypeScript の bundle です。Desktop release を作るときは、build machine で npm dependencies を lockfile どおりに入れ、web assets を生成してから release binary を作ります。

```bash
npm install
npm run build:desktop-web
cargo build --release --bin moyai-desktop
```

release executable は、同梱された `ui/desktop-web/dist` assets を Tauri production custom protocol で読み込みます。利用先の非接続端末には、npm、Rust toolchain、internet access、local dev server は不要です。必要な binary と assets を USB などで移し、`moyai-desktop.exe` を実行してください。

`moyai-desktop.exe` の cold start では、同梱の moyAI ロゴ splash を最低 5 秒表示します。その間に、設定ファイル、workspace、configured local LLM model catalog を確認します。設定が足りない場合や LLM 接続確認が通らない場合は、メインウィンドウを開いたあとに Settings または LLM URL を自動で表示します。

## Run

CLI から workspace を指定して実行する例です。

```bash
cargo run -- run --dir /path/to/workspace "このプロジェクトの主要モジュールを調べて要約してください。"
```

binary を install 済みなら、次のように使えます。

```bash
moyai run --dir /path/to/workspace "parser のテストを追加してください。"
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

release build では、CLI / TUI 用の `moyai.exe` と、Desktop App を直接起動する `moyai-desktop.exe` が生成されます。Windows では `moyai-desktop.exe` をダブルクリックするだけで Desktop App を起動できます。release では `npm run dev` や `127.0.0.1` の dev server は使いません。

Desktop runtime window / taskbar icon は `logo/fabicon/android-chrome-512x512.png` を使い、Windows executable resource には `logo/fabicon/moyai_app_icon.ico` を使います。

workspace を指定しない場合、Desktop App は workspace-free Quick Chat として起動します。Project 作業をしたい場合は、project 追加 control から folder を選ぶか、`--dir` を指定して起動します。

## 設定

moyAI は、global config、workspace config、environment variables、CLI overrides の順で設定を重ねます。

通常の app 初回起動時に global config が存在しない場合は、編集可能な default 値入りの設定ファイルを自動生成します。

- `%APPDATA%\midi-ai-labs\moyai\config\config.toml`

release folder に `.toml` を同梱する必要はありません。binary は `C:\tools\moyai\` のような安定した install directory に置き、user config と session data は Windows user profile 配下に保存します。

workspace config は次のどちらかに置けます。

- `moyai.toml`
- `.moyai/config.toml`

よく使う environment variables は次の通りです。

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

設定例です。

```toml
[model]
base_url = "http://127.0.0.1:1234"
# LM Studio でホストした qwen3.6-35b-a3b、lmstudio-community 版。
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

## Project local instructions

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

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
