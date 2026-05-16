<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="640">
</p>

# moyAI

**moyAI** は、ローカル LLM と閉域環境で使うことを前提に作っている Rust 製の coding agent です。

OpenAI 互換 API を持つローカル LLM サーバーにつなぎ、ワークスペースの調査、ファイル編集、shell 実行、セッション履歴、検証までを扱います。CLI、TUI、Desktop App は同じ Rust core の上で動きます。

[English README](README.md)

## どういうものか

最近の coding agent は、クラウド接続、外部ネットワーク、ホストされた model、online plugin ecosystem を前提にしているものが多いです。便利な一方で、機密コード、閉域ネットワーク、ローカル推論サーバー、再現性を重視する社内開発では、そのまま使いづらい場面があります。

moyAI は、そのあたりを最初から前提にした agent です。ローカルで動くこと、設定が明示的であること、ワークスペース内の作業内容を後から追えること、実行結果を deterministic な preflight / harness で確認できることを重視しています。

## できること

- OpenAI 互換 API 経由で local LLM に接続できます。
- `/v1/models` と LM Studio `/api/v1/models` metadata から model discovery できます。
- CLI、TUI、Tauri Desktop App を同じ core で使えます。
- workspace search、directory inspection、file read、diff-based edit、shell execution を扱えます。
- large file、binary、model checkpoint、structured document には read guard がかかります。
- 閉域 Docling Serve と HTTP MCP server を使った document workflow に対応しています。
- protocol-first history による session persistence と Markdown export ができます。
- vision-capable model では、CLI / Desktop から画像を添付できます。
- `default`、`auto_review`、`full_access` の permission preset を選べます。
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、local `SKILL.md` を読み込めます。
- `.moyai/commands/*.md` で project local な workflow command を定義できます。
- uncommitted changes や branch comparison の review entrypoint を使えます。
- runtime contract を確認する deterministic preflight / harness command を実行できます。

## 現在の状態

moyAI は、closed-network / local-LLM 前提の実用 coding agent として開発中です。この repository には core runtime、CLI、TUI、Desktop App、session storage、tool execution layer、provider metadata probing、harness / preflight infrastructure が入っています。

現時点では Windows を主な検証環境にしています。開発中の標準 model は、LM Studio でホストした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版です。ほかの model や provider profile も扱えるようにしていますが、まずはこの構成での安定性を重視しています。

## 必要なもの

- Rust 2024 edition に対応した Rust toolchain。
- OpenAI 互換 API を提供する local または到達可能な LLM endpoint。
- 任意: model metadata discovery 用の LM Studio。
- 任意: document workflow 用の Docling Serve または HTTP MCP server。

## Build

まず CLI / core を build する場合:

```bash
cargo build
```

Desktop UI は Tauri + TypeScript bundle です。Desktop release を作る場合は、build machine で npm dependencies を lockfile どおりに install し、web assets を生成してから release binary を作ります。

```bash
npm install
npm run build:desktop-web
cargo build --release --bin moyai-desktop
```

release executable は、Tauri production custom protocol で bundled `ui/desktop-web/dist` assets を読み込みます。利用先の非接続端末には、npm、Rust toolchain、internet access、local dev server は不要です。release executable / assets を USB などで移動して `moyai-desktop.exe` を実行できます。

`moyai-desktop.exe` の cold start では、同梱された moyAI ロゴ splash を最低 5 秒表示します。その間に、起動時点の設定ファイル、workspace、configured local LLM model catalog を確認します。設定が足りない場合や LLM 接続確認が通らない場合は、メインウィンドウ表示後に Settings または LLM URL を自動で開きます。

## Run

CLI から workspace を指定して実行する例:

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

release build では、CLI / TUI 用の `moyai.exe` と、Desktop App を直接起動する `moyai-desktop.exe` が生成されます。Windows では `moyai-desktop.exe` をダブルクリックするだけで Desktop App を起動できます。release では `npm run dev` や `127.0.0.1` の dev server は使いません。

Desktop runtime window / taskbar icon は `logo/fabicon/android-chrome-512x512.png` を使い、Windows executable resource は multi-size の `logo/fabicon/moyai_app_icon.ico` を使います。

workspace を指定しない場合、Desktop App は workspace-free Quick Chat として起動します。Project 作業は project 追加 control から folder を選ぶか、`--dir` を指定したときに開始します。

## 設定

moyAI は global config、workspace config、environment variables、CLI overrides の順に設定を読み込みます。

通常の app 初回起動時に global config が存在しない場合は、編集可能な default 値入りの設定ファイルを自動生成します。

- `%APPDATA%\midi-ai-labs\moyai\config\config.toml`

release folder に `.toml` を同梱する必要はありません。binary は `C:\tools\moyai\` のような安定した install directory に置き、user config と session data は Windows user profile 配下に保存します。

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

手元でよく使う check:

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
